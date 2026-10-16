//! A scripted Messages account, so the adapter can be driven through chatd's socket with no phone.
//!
//! `messages.signin.ctl` while linking: `scan`, `expire`, `fail`.
//! `messages.ctl` once linked:
//! `incoming WHO TEXT…`, `group WHO TEXT…`, `photo WHO`, `typing WHO`, `typing-stop WHO`,
//! `refuse WORDS…`, `drop`, `unlinked`.
//! WHO is `grace` or `alan`.

use super::{Drive, Event, Saved};
use crate::adapters::messages::bugle::{Chat, Note};
use crate::adapters::script;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const ME: &str = "phone-ada";
pub const GRACE: &str = "phone-grace";
pub const ALAN: &str = "phone-alan";
pub const CLUB: &str = "phone-club";
pub const LINK: &str = "https://support.google.com/messages/?p=web_computer#?c=ZmFrZS0x";
pub const LINK_AGAIN: &str = "https://support.google.com/messages/?p=web_computer#?c=ZmFrZS0y";

fn record(line: &str) {
    let path = protocol::support_dir().join("messages.calls");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

pub struct Fake {
    events: Sender<Event>,
    heard: Mutex<Option<Receiver<Event>>>,
    closed: Arc<AtomicBool>,
    refuse: Arc<Mutex<Option<String>>>,
}

impl Fake {
    pub fn open(_saved: &Saved) -> Fake {
        let (events, heard) = channel();
        let _ = events.send(Event::Ready);
        let _ = events.send(Event::Chats(vec![
            Chat { id: GRACE.into(), title: "Grace Ombra".into(), group: false, unread: false, mine: ME.into() },
            Chat { id: CLUB.into(), title: "Bridge club".into(), group: true, unread: true, mine: ME.into() },
        ]));
        let fake = Fake { events, heard: Mutex::new(Some(heard)), closed: Arc::default(), refuse: Arc::default() };
        let (events, closed, refuse) = (fake.events.clone(), fake.closed.clone(), fake.refuse.clone());
        let control = script::control("messages.ctl");
        let _ = std::thread::Builder::new().name("messages-fake".into()).spawn(move || {
            script::follow(&control, |line| {
                if closed.load(Ordering::Relaxed) {
                    return false;
                }
                let Some(line) = line.map(str::trim) else {
                    return true;
                };
                if let Some(words) = line.strip_prefix("refuse ") {
                    *refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(words.to_owned());
                    return true;
                }
                for event in steer(line) {
                    let _ = events.send(event);
                }
                true
            });
        });
        fake
    }
}

fn who(word: &str) -> Option<(&'static str, &'static str)> {
    match word {
        "grace" => Some((GRACE, "Grace Ombra")),
        "alan" => Some((ALAN, "Alan Ferro")),
        _ => None,
    }
}

fn steer(line: &str) -> Vec<Event> {
    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or("");
    let rest = |skip| line.split_whitespace().skip(skip).collect::<Vec<_>>().join(" ");
    match command {
        "incoming" | "group" | "photo" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let group = command == "group";
            let conversation = if group { CLUB } else { author };
            let text = if command == "photo" { "File".into() } else { rest(2) };
            vec![Event::Message(Note {
                id: format!("m-{author}-{}", text.len()),
                conversation: conversation.into(),
                author: author.into(),
                name: name.into(),
                sent: 1_791_892_800,
                text,
                outgoing: false,
                file: command == "photo",
            })]
        }
        "typing" | "typing-stop" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            vec![Event::Typing { conversation: author.into(), by: author.into(), name: name.into(), started: command == "typing" }]
        }
        "drop" => vec![Event::Closed("the connection dropped".into())],
        "unlinked" => vec![Event::Ended("This device was removed in Messages.".into())],
        _ => Vec::new(),
    }
}

impl Drive for Fake {
    fn events(&self) -> Option<Receiver<Event>> {
        self.heard.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }
    fn me(&self) -> (String, String) {
        (ME.into(), "Ada Quill".into())
    }
    fn refresh(&self) -> Result<(), String> {
        record("list");
        Ok(())
    }
    fn history(&self, conversation: &str) -> Result<Vec<Note>, String> {
        record(&format!("history {conversation}"));
        Ok(Vec::new())
    }
    fn send(&self, conversation: &str, text: &str) -> Result<String, String> {
        if let Some(words) = self.refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            return Err(words);
        }
        record(&format!("send {conversation} {text}"));
        Ok(format!("sent-{conversation}"))
    }
    fn mark_read(&self, conversation: &str, message: &str) -> Result<(), String> {
        record(&format!("read {conversation} {message}"));
        Ok(())
    }
    fn typing(&self, conversation: &str) -> Result<(), String> {
        record(&format!("typing {conversation}"));
        Ok(())
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

/// Links when the test says the phone scanned the code.
pub struct FakeLinker;

impl FakeLinker {
    pub fn link(show: &dyn Fn(String), cancelled: &AtomicBool) -> Result<String, String> {
        let control = script::control("messages.signin.ctl");
        show(LINK.to_owned());
        let mut outcome = None;
        script::follow(&control, |line| {
            if cancelled.load(Ordering::Relaxed) {
                outcome = Some(Err("cancelled".into()));
                return false;
            }
            match line.map(str::trim) {
                Some("scan") => {
                    let session = serde_json::json!({ "account": ME, "name": "Ada Quill" });
                    outcome = Some(Ok(session.to_string()));
                    false
                }
                Some("expire") => {
                    show(LINK_AGAIN.to_owned());
                    true
                }
                Some("fail") => {
                    outcome = Some(Err("Messages could not pair.".into()));
                    false
                }
                _ => true,
            }
        });
        outcome.unwrap_or_else(|| Err("the pairing was interrupted".into()))
    }
}
