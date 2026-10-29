//! WinRT toasts for an unpackaged Win32 app.
//!
//! The AppUserModelID is what a toast is shown under: its name and icon come from the Start
//! menu shortcut that carries the ID (`packaging/windows/tinyclient.iss`), or from
//! `HKCU\Software\Classes\AppUserModelId\<id>` ([`register`]), which is enough on its own.
//!
//! Activation. Without a COM activator registered for the ID, Windows delivers a click or a
//! button only as the `Activated` event of the `ToastNotification` object, in the process that
//! posted it and while that process lives. chatd posts, keeps each object (and so its handler)
//! until it is replaced or withdrawn, and clears the toasts an earlier chatd left, whose
//! handlers are gone, when it starts.

use super::{activation, toast_xml, App, Note, Sink, Tell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use windows::core::{Interface, HSTRING, IInspectable};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{IPropertyValue, TypedEventHandler};
use windows::UI::Notifications::{NotificationSetting, ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};
use windows::Win32::System::RemoteDesktop::{
    WTSFreeMemory, WTSQuerySessionInformationW, WTSSessionInfoEx, WTSINFOEXW, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION,
    WTS_SESSIONSTATE_LOCK,
};

/// Every toast is in this group, so a tag only has to be unique within it.
const GROUP: &str = "tinyclient";

/// A toast's tag: at most 64 characters, so a hash of the note's id.
fn tag(id: &str) -> HSTRING {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    HSTRING::from(format!("{:016x}", hasher.finish()))
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Writes the ID's name and icon under HKCU, as an installed shortcut would make them known.
pub fn register(app: &App, replace: bool) -> Result<(), String> {
    let key = utf16(&format!("Software\\Classes\\AppUserModelId\\{}", app.id));
    if !replace {
        let name = utf16("DisplayName");
        let mut bytes = 0u32;
        // SAFETY: asks only the size of a value that may not exist; no buffer is written.
        let found = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, windows::core::PCWSTR(key.as_ptr()), windows::core::PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut bytes))
        };
        if found.is_ok() {
            return Ok(());
        }
    }
    let mut values = vec![("DisplayName", app.name.clone())];
    if let Some(icon) = app.icon.as_ref().filter(|icon| icon.is_file()) {
        values.push(("IconUri", icon.display().to_string()));
    }
    for (name, value) in values {
        let (name, value) = (utf16(name), utf16(&value));
        // SAFETY: both strings are NUL-terminated and outlive the call; the size is in bytes.
        let done = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                windows::core::PCWSTR(key.as_ptr()),
                windows::core::PCWSTR(name.as_ptr()),
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            )
        };
        if done.is_err() {
            return Err(format!("the AppUserModelID could not be registered: {done:?}"));
        }
    }
    Ok(())
}

/// Whether this session's screen is locked, as Terminal Services reports it.
pub fn locked() -> bool {
    let mut buffer = windows::core::PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: the buffer is the system's, read as the WTSINFOEXW it is for this class, and freed
    // with WTSFreeMemory once read.
    unsafe {
        if WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), WTS_CURRENT_SESSION, WTSSessionInfoEx, &mut buffer, &mut bytes).is_err() || buffer.is_null() {
            return false;
        }
        let locked = if bytes as usize >= std::mem::size_of::<WTSINFOEXW>() {
            let info = &*(buffer.0 as *const WTSINFOEXW);
            info.Level == 1 && info.Data.WTSInfoExLevel1.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK
        } else {
            false
        };
        WTSFreeMemory(buffer.0.cast());
        locked
    }
}

pub struct Toasts {
    id: HSTRING,
    notifier: Option<ToastNotifier>,
    tell: Tell,
    /// The toasts shown, by note id, kept so their `Activated` handlers stay alive.
    shown: HashMap<String, ToastNotification>,
    /// Why there is no notifier, when there is none.
    broken: String,
}

impl Toasts {
    pub fn new(app: &App, tell: Tell) -> Toasts {
        // SAFETY: joins this thread to the multithreaded apartment; a second call on a thread
        // already in it is harmless.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let id = HSTRING::from(app.id.as_str());
        let (notifier, broken) = match ToastNotificationManager::CreateToastNotifierWithId(&id) {
            Ok(notifier) => (Some(notifier), String::new()),
            Err(err) => (None, format!("Windows refused tinyclient's notifications: {}", err.message())),
        };
        // Toasts left by an earlier run cannot be acted on any more: their handlers died with it.
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.ClearWithId(&id);
        }
        Toasts { id, notifier, tell, shown: HashMap::new(), broken }
    }

    fn toast(&self, note: &Note) -> windows::core::Result<ToastNotification> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(toast_xml(note)))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.SetTag(&tag(&note.id))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        let tell = self.tell.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, args| {
            let Some(args) = args.as_ref().and_then(|args| args.cast::<ToastActivatedEventArgs>().ok()) else {
                return Ok(());
            };
            let arguments = args.Arguments().map(|a| a.to_string()).unwrap_or_default();
            let reply = args
                .UserInput()
                .ok()
                .and_then(|input| input.Lookup(&HSTRING::from("reply")).ok())
                .and_then(|value| value.cast::<IPropertyValue>().ok())
                .and_then(|value| value.GetString().ok())
                .map(|text| text.to_string());
            if let Some(done) = activation(&arguments, reply) {
                tell(done);
            }
            Ok(())
        }))?;
        Ok(toast)
    }
}

/// The words for a notifier that is turned off.
fn refused(setting: NotificationSetting) -> Option<&'static str> {
    match setting {
        NotificationSetting::Enabled => None,
        NotificationSetting::DisabledForApplication => Some("Notifications are turned off for tinyclient in Windows Settings › System › Notifications."),
        NotificationSetting::DisabledForUser => Some("Notifications are turned off in Windows Settings › System › Notifications."),
        NotificationSetting::DisabledByGroupPolicy => Some("Notifications are turned off by your organisation's policy."),
        _ => Some("Windows won't show tinyclient's notifications."),
    }
}

impl Sink for Toasts {
    fn post(&mut self, note: &Note) -> Result<(), String> {
        let Some(notifier) = &self.notifier else {
            return Err(self.broken.clone());
        };
        if let Some(words) = notifier.Setting().ok().and_then(refused) {
            return Err(words.to_owned());
        }
        let toast = self.toast(note).map_err(|err| format!("The notification could not be made: {}", err.message()))?;
        // A toast with the tag and group of one on screen replaces it in place.
        notifier.Show(&toast).map_err(|err| format!("Windows did not show the notification: {}", err.message()))?;
        self.shown.insert(note.id.clone(), toast);
        Ok(())
    }

    fn withdraw(&mut self, id: &str) {
        if self.shown.remove(id).is_none() {
            return;
        }
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupedTagWithId(&tag(id), &HSTRING::from(GROUP), &self.id);
        }
    }

    fn listed(&self, id: &str) -> bool {
        let Ok(history) = ToastNotificationManager::History() else {
            return false;
        };
        let Ok(listed) = history.GetHistoryWithId(&self.id) else {
            return false;
        };
        let wanted = tag(id);
        (&listed).into_iter().any(|toast| toast.Tag().is_ok_and(|tag| tag == wanted))
    }
}
