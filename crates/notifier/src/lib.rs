//! The system's notifications, for chatd, and the taskbar button or Dock icon, for the window
//! (`docs/design/06-live-signals.md` §6 and §8). What to post is chatd's to decide
//! (`chatd/src/notify.rs`); this crate only shows it, withdraws it and reports what the user
//! did with it. Every backend takes the same [`Note`], so the rules are tested once, against
//! [`Fake`].
//!
//! - Windows: WinRT toasts under tinyclient's AppUserModelID. An unpackaged app needs the ID
//!   registered: the installer's Start menu shortcut carries it, and [`App::register`] writes
//!   it under HKCU so a build that was never installed posts too. Clicks and buttons arrive as
//!   the toast's `Activated` event in the process that posted it, which is chatd, running for
//!   as long as the toasts it posted can be seen; the ones left from an earlier run are cleared
//!   at start. Toasts are posted silent: an unpackaged app can only name the system's sounds,
//!   so chatd plays ours itself (`mediad --sound`).
//! - macOS: UNUserNotificationCenter, only inside the app bundle (it refuses a bare binary);
//!   elsewhere nothing is posted and the reason is logged. The bundle carries the sound files.
//! - Linux: org.freedesktop.Notifications, with `suppress-sound` as on Windows.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, target_os = "macos"))]
pub mod taskbar;

/// What a notification is for, which decides its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Reply, with a text field, and Mark as read.
    Message,
    /// Mark as read, and no reply. The conversation does not take posts.
    ReadOnly,
    /// Accept and Decline, shown as a call.
    Call,
    /// No buttons: the test notification, a sign-out.
    Plain,
}

/// One notification. Posting a note with the `id` of one still shown replaces it in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Stable for what it is about (one conversation, one ring), so a newer message replaces
    /// the notification and reading the conversation withdraws it.
    pub id: String,
    pub title: String,
    /// Where it was said: "#platform · Northwind". Empty for a direct message.
    pub subtitle: String,
    pub body: String,
    pub kind: Kind,
    /// The reply field's placeholder: "Reply to Ines".
    pub reply_hint: String,
    /// The sound the system should play, as a bundled file's stem, on the systems that play
    /// it themselves ([`SYSTEM_PLAYS_SOUND`]). Everywhere else notes are silent.
    pub sound: Option<String>,
    /// What the system should show while the screen is locked, when it decides that itself
    /// (macOS's hidden-preview placeholder): "New message".
    pub locked_body: String,
}

/// What the user did with a notification, by its [`Note::id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// Clicked: open the app there.
    Open(String),
    Reply { id: String, text: String },
    MarkRead(String),
    Accept(String),
    Decline(String),
}

/// Where activations go. Called on whatever thread the system uses.
pub type Tell = Arc<dyn Fn(Activation) + Send + Sync>;

/// Shows and withdraws notifications.
pub trait Sink: Send {
    /// Posts `note`, replacing one with the same id. An error is the system's refusal in words.
    fn post(&mut self, note: &Note) -> Result<(), String>;
    /// Removes it from the screen and the notification centre. Unknown ids are ignored.
    fn withdraw(&mut self, id: &str);
    /// Whether the system still lists it: for the test notification's answer.
    fn listed(&self, id: &str) -> bool;
}

/// Whether the system plays [`Note::sound`] itself. Where it does not, chatd plays it.
pub const SYSTEM_PLAYS_SOUND: bool = cfg!(target_os = "macos");

/// Who posts: the ID the system knows the app by, and what it shows for it.
#[derive(Debug, Clone)]
pub struct App {
    /// Windows' AppUserModelID; macOS reads the bundle's own; the Linux desktop entry's name.
    pub id: String,
    pub name: String,
    /// An icon file for the system to show beside it (Windows: `.ico` or `.png`).
    pub icon: Option<PathBuf>,
}

impl App {
    /// Makes the ID known to the system where an unpackaged app must (Windows, HKCU). With
    /// `replace` off, an ID already registered (by the installer) is left as it is. Does
    /// nothing elsewhere. Safe to repeat.
    pub fn register(&self, replace: bool) -> Result<(), String> {
        #[cfg(windows)]
        return windows::register(self, replace);
        #[cfg(not(windows))]
        {
            let _ = replace;
            Ok(())
        }
    }
}

/// The system's notifications for `app`. Never fails: without a notification service the
/// sink refuses each post with the reason.
pub fn system(app: &App, tell: Tell) -> Box<dyn Sink> {
    #[cfg(windows)]
    return Box::new(windows::Toasts::new(app, tell));
    #[cfg(target_os = "macos")]
    return Box::new(macos::Center::new(app, tell));
    #[cfg(all(unix, not(target_os = "macos")))]
    return Box::new(linux::Bus::new(app, tell));
    #[allow(unreachable_code)]
    {
        let _ = (app, tell);
        Box::new(Unsupported)
    }
}

/// Lets another process (the window, raised for a clicked notification) take the foreground,
/// which Windows allows only to the process the user just acted in. Nothing elsewhere.
pub fn allow_foreground() {
    #[cfg(windows)]
    windows::allow_foreground();
}

/// Whether the session's screen is locked now. False where it cannot be told.
pub fn screen_locked() -> bool {
    #[cfg(windows)]
    return windows::locked();
    #[cfg(not(windows))]
    false
}

#[allow(dead_code)]
struct Unsupported;

impl Sink for Unsupported {
    fn post(&mut self, _: &Note) -> Result<(), String> {
        Err("This system has no notifications tinyclient can post.".to_owned())
    }
    fn withdraw(&mut self, _: &str) {}
    fn listed(&self, _: &str) -> bool {
        false
    }
}

/// What a [`Fake`] was asked, in order, and what it shows now.
#[derive(Debug, Default)]
pub struct Log {
    pub posted: Vec<Note>,
    pub withdrawn: Vec<String>,
    pub shown: BTreeMap<String, Note>,
    /// Refuse every post with this, as a system with notifications turned off does.
    pub refuse: Option<String>,
}

/// A sink that remembers instead of showing, for tests.
#[derive(Clone, Default)]
pub struct Fake(pub Arc<Mutex<Log>>);

impl Fake {
    pub fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Sink for Fake {
    fn post(&mut self, note: &Note) -> Result<(), String> {
        let mut log = self.log();
        if let Some(refused) = log.refuse.clone() {
            return Err(refused);
        }
        log.posted.push(note.clone());
        log.shown.insert(note.id.clone(), note.clone());
        Ok(())
    }

    fn withdraw(&mut self, id: &str) {
        let mut log = self.log();
        if log.shown.remove(id).is_some() {
            log.withdrawn.push(id.to_owned());
        }
    }

    fn listed(&self, id: &str) -> bool {
        self.log().shown.contains_key(id)
    }
}

/// Text as XML character data or an attribute value.
#[allow(dead_code)]
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters other than tab and newline are not allowed in XML 1.0.
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// A toast's XML: title, body, where, buttons, silent. Built here so it is tested anywhere.
#[allow(dead_code)]
fn toast_xml(note: &Note) -> String {
    let id = escape(&note.id);
    let scenario = if note.kind == Kind::Call { " scenario=\"incomingCall\"" } else { "" };
    let mut xml = format!("<toast launch=\"open:{id}\" activationType=\"foreground\"{scenario}><visual><binding template=\"ToastGeneric\">");
    xml.push_str(&format!("<text>{}</text>", escape(&note.title)));
    if !note.body.is_empty() {
        xml.push_str(&format!("<text>{}</text>", escape(&note.body)));
    }
    if !note.subtitle.is_empty() {
        xml.push_str(&format!("<text placement=\"attribution\">{}</text>", escape(&note.subtitle)));
    }
    xml.push_str("</binding></visual>");
    match note.kind {
        Kind::Message => xml.push_str(&format!(
            "<actions><input id=\"reply\" type=\"text\" placeHolderContent=\"{}\"/>\
             <action content=\"Send\" arguments=\"reply:{id}\" hint-inputId=\"reply\" activationType=\"foreground\"/>\
             <action content=\"Mark as read\" arguments=\"read:{id}\" activationType=\"foreground\"/></actions>",
            escape(&note.reply_hint)
        )),
        Kind::ReadOnly => xml.push_str(&format!(
            "<actions><action content=\"Mark as read\" arguments=\"read:{id}\" activationType=\"foreground\"/></actions>"
        )),
        Kind::Call => xml.push_str(&format!(
            "<actions><action content=\"Accept\" arguments=\"accept:{id}\" activationType=\"foreground\"/>\
             <action content=\"Decline\" arguments=\"decline:{id}\" activationType=\"foreground\"/></actions>"
        )),
        Kind::Plain => {}
    }
    xml.push_str("<audio silent=\"true\"/></toast>");
    xml
}

/// A toast's `arguments` (or `launch`) back as what the user did.
#[allow(dead_code)]
fn activation(arguments: &str, reply: Option<String>) -> Option<Activation> {
    let (verb, id) = arguments.split_once(':')?;
    let id = id.to_owned();
    Some(match verb {
        "open" => Activation::Open(id),
        "reply" => match reply.filter(|text| !text.trim().is_empty()) {
            Some(text) => Activation::Reply { id, text },
            // Send with nothing typed: open it instead, so the press is not lost.
            None => Activation::Open(id),
        },
        "read" => Activation::MarkRead(id),
        "accept" => Activation::Accept(id),
        "decline" => Activation::Decline(id),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(kind: Kind) -> Note {
        Note {
            id: "m:1".into(),
            title: "Ines <Varga>".into(),
            subtitle: "#platform · Northwind".into(),
            body: "Tom & Jerry said \"hi\"\u{7}".into(),
            kind,
            reply_hint: "Reply to Ines".into(),
            sound: None,
            locked_body: "New message".into(),
        }
    }

    #[test]
    fn a_toast_escapes_what_people_wrote_and_is_always_silent() {
        let xml = toast_xml(&note(Kind::Message));
        assert!(xml.contains("<text>Ines &lt;Varga&gt;</text>"));
        assert!(xml.contains("<text>Tom &amp; Jerry said &quot;hi&quot;</text>"), "{xml}");
        assert!(xml.contains("placement=\"attribution\">#platform · Northwind"));
        assert!(xml.contains("arguments=\"reply:m:1\" hint-inputId=\"reply\""));
        assert!(xml.contains("arguments=\"read:m:1\""));
        assert!(xml.ends_with("<audio silent=\"true\"/></toast>"));
        let call = toast_xml(&note(Kind::Call));
        assert!(call.contains("scenario=\"incomingCall\"") && call.contains("accept:m:1") && call.contains("decline:m:1"));
        assert!(!toast_xml(&note(Kind::Plain)).contains("<actions>"));
        let locked = toast_xml(&note(Kind::ReadOnly));
        assert!(locked.contains("arguments=\"read:m:1\""), "{locked}");
        assert!(!locked.contains("<input") && !locked.contains("reply:"), "{locked}");
    }

    #[test]
    fn what_the_user_did_is_read_back_from_the_arguments() {
        assert_eq!(activation("open:m:1", None), Some(Activation::Open("m:1".into())));
        assert_eq!(activation("reply:m:1", Some("on my way".into())), Some(Activation::Reply { id: "m:1".into(), text: "on my way".into() }));
        assert_eq!(activation("reply:m:1", Some("  ".into())), Some(Activation::Open("m:1".into())));
        assert_eq!(activation("read:c:9", None), Some(Activation::MarkRead("c:9".into())));
        assert_eq!(activation("accept:r:2", None), Some(Activation::Accept("r:2".into())));
        assert_eq!(activation("decline:r:2", None), Some(Activation::Decline("r:2".into())));
        assert_eq!(activation("dance:r:2", None), None);
        assert_eq!(activation("", None), None);
    }

    #[test]
    fn the_fake_replaces_by_id_and_withdraws_what_it_shows() {
        let fake = Fake::default();
        let mut sink: Box<dyn Sink> = Box::new(fake.clone());
        sink.post(&note(Kind::Message)).unwrap();
        let mut newer = note(Kind::Message);
        newer.body = "second".into();
        sink.post(&newer).unwrap();
        assert!(sink.listed("m:1"));
        assert_eq!(fake.log().shown["m:1"].body, "second");
        sink.withdraw("m:1");
        sink.withdraw("m:1");
        assert_eq!(fake.log().withdrawn, ["m:1"]);
        fake.log().refuse = Some("off".into());
        assert_eq!(sink.post(&note(Kind::Plain)), Err("off".into()));
    }
}
