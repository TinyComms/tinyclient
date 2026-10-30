//! UNUserNotificationCenter. Only a process inside an app bundle may use it: elsewhere the
//! framework raises, so a bare binary (`cargo run -p chatd`) is refused up front and posts
//! nothing. Inside an ad-hoc signed bundle the system may still decline; the reason is logged.
//!
//! Replies, Mark as Read, Accept and Decline come back through the center's delegate. macOS
//! delivers them to the process that registered it when that process is running; a click with
//! chatd gone launches the bundle's main program, the window.

use super::{Activation, App, Kind, Note, Sink, Tell};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction, UNNotificationActionOptions,
    UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNTextInputNotificationAction, UNTextInputNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::sync::mpsc;
use std::time::Duration;

const MESSAGE: &str = "message";
const READONLY: &str = "readonly";
const CALL: &str = "call";

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TinyclientNotificationDelegate"]
    #[ivars = Tell]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, done: &block2::DynBlock<dyn Fn()>) {
            let id = response.notification().request().identifier().to_string();
            let action = response.actionIdentifier().to_string();
            // SAFETY: a static the framework defines.
            let default = unsafe { UNNotificationDefaultActionIdentifier }.to_string();
            let reply = response.downcast_ref::<UNTextInputNotificationResponse>().map(|text| text.userText().to_string());
            let done_with = match action.as_str() {
                "reply" => reply.filter(|text| !text.trim().is_empty()).map(|text| Activation::Reply { id: id.clone(), text }),
                "read" => Some(Activation::MarkRead(id.clone())),
                "accept" => Some(Activation::Accept(id.clone())),
                "decline" => Some(Activation::Decline(id.clone())),
                _ if action == default => Some(Activation::Open(id.clone())),
                _ => None,
            };
            if let Some(activation) = done_with {
                (self.ivars())(activation);
            }
            done.call(());
        }

        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(&self, _center: &UNUserNotificationCenter, _notification: &UNNotification, done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            // chatd decided already, the window being on another conversation included.
            done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List | UNNotificationPresentationOptions::Sound,));
        }
    }
);

impl Delegate {
    fn new(tell: Tell) -> Retained<Delegate> {
        let this = Delegate::alloc().set_ivars(tell);
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

pub struct Center {
    center: Option<Retained<UNUserNotificationCenter>>,
    /// Kept alive: the center holds its delegate weakly.
    _delegate: Option<Retained<Delegate>>,
    broken: String,
}

// SAFETY: UNUserNotificationCenter is thread-safe, and the delegate is only read by it.
unsafe impl Send for Center {}

fn category(id: &str, actions: &[Retained<UNNotificationAction>], hidden: &str) -> Retained<UNNotificationCategory> {
    let actions: Vec<&UNNotificationAction> = actions.iter().map(|action| &**action).collect();
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_hiddenPreviewsBodyPlaceholder_options(
        &NSString::from_str(id),
        &NSArray::from_slice(&actions),
        &NSArray::new(),
        &NSString::from_str(hidden),
        UNNotificationCategoryOptions::empty(),
    )
}

impl Center {
    pub fn new(_app: &App, tell: Tell) -> Center {
        // The framework raises an exception, which aborts, for a process outside a bundle.
        let bundled = NSBundle::mainBundle().bundleIdentifier().is_some() && NSBundle::mainBundle().bundlePath().to_string().ends_with(".app");
        if !bundled {
            return Center {
                center: None,
                _delegate: None,
                broken: "Notifications need tinyclient.app: this chatd runs outside it.".to_owned(),
            };
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new(tell);
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let reply = UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
            &NSString::from_str("reply"),
            &NSString::from_str("Reply"),
            UNNotificationActionOptions::empty(),
            &NSString::from_str("Send"),
            &NSString::from_str("Reply"),
        );
        let read = || {
            UNNotificationAction::actionWithIdentifier_title_options(&NSString::from_str("read"), &NSString::from_str("Mark as Read"), UNNotificationActionOptions::empty())
        };
        let accept = UNNotificationAction::actionWithIdentifier_title_options(&NSString::from_str("accept"), &NSString::from_str("Accept"), UNNotificationActionOptions::Foreground);
        let decline = UNNotificationAction::actionWithIdentifier_title_options(&NSString::from_str("decline"), &NSString::from_str("Decline"), UNNotificationActionOptions::Destructive);
        let message = category(MESSAGE, &[Retained::into_super(reply), read()], "New message");
        let readonly = category(READONLY, &[read()], "New message");
        let call = category(CALL, &[accept, decline], "Incoming call");
        center.setNotificationCategories(&NSSet::from_retained_slice(&[message, readonly, call]));
        let asked = RcBlock::new(|granted: Bool, error: *mut NSError| {
            if !granted.as_bool() {
                // SAFETY: the framework passes a valid error or null.
                let why = unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string()).unwrap_or_default();
                eprintln!("chatd: notifications not allowed {why}");
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge, &asked);
        Center { center: Some(center), _delegate: Some(delegate), broken: String::new() }
    }
}

impl Sink for Center {
    fn post(&mut self, note: &Note) -> Result<(), String> {
        let Some(center) = &self.center else {
            return Err(self.broken.clone());
        };
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&note.title));
        content.setSubtitle(&NSString::from_str(&note.subtitle));
        content.setBody(&NSString::from_str(&note.body));
        content.setThreadIdentifier(&NSString::from_str(&note.id));
        match note.kind {
            Kind::Message => content.setCategoryIdentifier(&NSString::from_str(MESSAGE)),
            Kind::ReadOnly => content.setCategoryIdentifier(&NSString::from_str(READONLY)),
            Kind::Call => content.setCategoryIdentifier(&NSString::from_str(CALL)),
            Kind::Plain => {}
        }
        // The bundle's Resources hold `<stem>.caf`.
        let sound = note.sound.as_ref().map(|stem| UNNotificationSound::soundNamed(&NSString::from_str(&format!("{stem}.caf"))));
        content.setSound(sound.as_deref());
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&note.id), &content, None);
        let id = note.id.clone();
        let added = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: the framework passes a valid error or null.
            if let Some(error) = unsafe { error.as_ref() } {
                eprintln!("chatd: notification {id} not posted: {}", error.localizedDescription());
            }
        });
        // The same identifier replaces the one delivered.
        center.addNotificationRequest_withCompletionHandler(&request, Some(&added));
        Ok(())
    }

    fn withdraw(&mut self, id: &str) {
        if let Some(center) = &self.center {
            let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
            center.removeDeliveredNotificationsWithIdentifiers(&ids);
            center.removePendingNotificationRequestsWithIdentifiers(&ids);
        }
    }

    fn listed(&self, id: &str) -> bool {
        let Some(center) = &self.center else { return false };
        let (tell, told) = mpsc::channel();
        let wanted = id.to_owned();
        let found = RcBlock::new(move |delivered: std::ptr::NonNull<NSArray<UNNotification>>| {
            // SAFETY: the framework passes a valid array for the length of the call.
            let delivered = unsafe { delivered.as_ref() };
            let _ = tell.send(delivered.iter().any(|note| note.request().identifier().to_string() == wanted));
        });
        center.getDeliveredNotificationsWithCompletionHandler(&found);
        told.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
    }
}
