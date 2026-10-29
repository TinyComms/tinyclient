//! org.freedesktop.Notifications on the session bus. Posted with `suppress-sound`, as chatd
//! plays the chosen sound itself; Reply is the `inline-reply` action that KDE and GNOME show
//! as a text field, and a server without it shows Mark as read alone. Clicks and replies come
//! back as `ActionInvoked` and `NotificationReplied`, read on a thread of their own.

use super::{escape, Activation, App, Kind, Note, Sink, Tell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// The server's ids for ours, both ways.
#[derive(Default)]
struct Ids {
    server: HashMap<String, u32>,
    ours: HashMap<u32, String>,
}

pub struct Bus {
    proxy: Option<Proxy<'static>>,
    app: App,
    ids: Arc<Mutex<Ids>>,
    broken: String,
}

fn lock(ids: &Mutex<Ids>) -> std::sync::MutexGuard<'_, Ids> {
    ids.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Follows one signal, `(id, word)`, and hands each to `act`.
fn follow(proxy: Proxy<'static>, signal: &'static str, ids: Arc<Mutex<Ids>>, act: impl Fn(String, String) -> Option<Activation> + Send + 'static, tell: Tell) {
    let _ = std::thread::Builder::new().name(format!("notify-{signal}")).spawn(move || {
        let Ok(signals) = proxy.receive_signal(signal) else { return };
        for message in signals {
            let Ok((server, word)) = message.body().deserialize::<(u32, String)>() else { continue };
            let Some(id) = lock(&ids).ours.get(&server).cloned() else { continue };
            if let Some(done) = act(id, word) {
                tell(done);
            }
        }
    });
}

impl Bus {
    pub fn new(app: &App, tell: Tell) -> Bus {
        let ids = Arc::new(Mutex::new(Ids::default()));
        let proxy = Connection::session().and_then(|bus| Proxy::new(&bus, NAME, PATH, NAME));
        let proxy = match proxy {
            Ok(proxy) => proxy,
            Err(err) => return Bus { proxy: None, app: app.clone(), ids, broken: format!("No notification service on the session bus: {err}") },
        };
        follow(proxy.clone(), "ActionInvoked", ids.clone(), |id, action| match action.as_str() {
            "default" => Some(Activation::Open(id)),
            "read" => Some(Activation::MarkRead(id)),
            "accept" => Some(Activation::Accept(id)),
            "decline" => Some(Activation::Decline(id)),
            _ => None,
        }, tell.clone());
        follow(proxy.clone(), "NotificationReplied", ids.clone(), |id, text| (!text.trim().is_empty()).then_some(Activation::Reply { id, text }), tell);
        Bus { proxy: Some(proxy), app: app.clone(), ids, broken: String::new() }
    }
}

impl Sink for Bus {
    fn post(&mut self, note: &Note) -> Result<(), String> {
        let Some(proxy) = &self.proxy else {
            return Err(self.broken.clone());
        };
        let replaces = lock(&self.ids).server.get(&note.id).copied().unwrap_or(0);
        let actions: Vec<&str> = match note.kind {
            Kind::Message => vec!["default", "Open", "inline-reply", "Reply", "read", "Mark as read"],
            Kind::Call => vec!["default", "Open", "accept", "Accept", "decline", "Decline"],
            Kind::Plain => vec!["default", "Open"],
        };
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("suppress-sound", Value::from(true));
        hints.insert("desktop-entry", Value::from(self.app.id.as_str()));
        hints.insert("category", Value::from(if note.kind == Kind::Call { "call.incoming" } else { "im.received" }));
        hints.insert("x-kde-reply-placeholder-text", Value::from(note.reply_hint.as_str()));
        if note.kind == Kind::Call {
            hints.insert("urgency", Value::from(2u8));
        }
        let body = match note.subtitle.is_empty() {
            true => escape(&note.body),
            false => format!("{}\n{}", escape(&note.subtitle), escape(&note.body)),
        };
        let icon = self.app.icon.as_ref().map(|icon| icon.display().to_string()).unwrap_or_default();
        let posted: u32 = proxy
            .call("Notify", &(self.app.name.as_str(), replaces, icon.as_str(), note.title.as_str(), body.as_str(), actions, hints, -1i32))
            .map_err(|err| format!("The notification service refused it: {err}"))?;
        let mut ids = lock(&self.ids);
        ids.ours.remove(&replaces);
        ids.server.insert(note.id.clone(), posted);
        ids.ours.insert(posted, note.id.clone());
        Ok(())
    }

    fn withdraw(&mut self, id: &str) {
        let Some(server) = lock(&self.ids).server.remove(id) else { return };
        lock(&self.ids).ours.remove(&server);
        if let Some(proxy) = &self.proxy {
            let _: Result<(), _> = proxy.call("CloseNotification", &(server,));
        }
    }

    fn listed(&self, id: &str) -> bool {
        // The specification has no way to ask; what was posted and not closed counts.
        lock(&self.ids).server.contains_key(id)
    }
}
