//! A wire with a script behind it and no network: a small invented account that takes what the
//! adapter sends and says what a test tells it to. It exists so the adapter can be driven through
//! chatd's socket (`tests/signal.rs`) with no Signal library linked; it is reachable only in a
//! scratch directory with `TINYCLIENT_SIGNAL_FAKE` set, never from a real session.
//!
//! A test steers it by appending lines to a file in the scratch directory (`adapters/script.rs`):
//! `signal.signin.ctl` while linking (`scan`, `expire`, `fail`), `signal.ctl` once linked:
//!
//! | Line | What Signal then says |
//! | --- | --- |
//! | `incoming WHO SENT TEXT…` | WHO wrote TEXT to us, sent at SENT (ms) |
//! | `group WHO SENT TEXT…` | the same, in the group |
//! | `mention WHO SENT TEXT…` | the same, in the group, naming us |
//! | `photo WHO SENT` | WHO sent a photo with no caption |
//! | `quote WHO SENT QUOTED TEXT…` | WHO answered our message sent at QUOTED |
//! | `mine SENT TEXT…` | we wrote TEXT to Grace from the phone |
//! | `note SENT TEXT…` | we wrote TEXT to Note to Self from the phone |
//! | `edit WHO TARGET TEXT…`, `delete WHO TARGET` | WHO changed or took back their message |
//! | `react WHO AUTHOR TARGET EMOJI`, `unreact …` | WHO reacted to AUTHOR's message |
//! | `read WHO SENT` | we read WHO's message on the phone |
//! | `seen WHO SENT` | WHO read our message |
//! | `typing WHO`, `typing-stop WHO` | WHO is typing to us, or stopped |
//! | `refuse WORDS…` | the next send is refused, in those words |
//! | `drop`, `unlinked` | the connection ends; the phone removed this device |
//!
//! WHO is `grace` or `alan`. Every request it is sent is appended to `signal.calls`, one line
//! each, so a test can see what the network would have heard.
//!
//! The account: Ada Quill (us); Grace Ombra and Alan Ferro, both contacts; the group "Bridge
//! club" with all three of us in it.

use super::wire::{Body, Contact, Event, Fault, GroupInfo, Linked, Linker, Peer, Quote, Text, Wire};
use super::Saved;
use crate::adapters::script;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const ME: &str = "a1a1a1a1-0000-4000-8000-000000000001";
pub const GRACE: &str = "b2b2b2b2-0000-4000-8000-000000000002";
pub const ALAN: &str = "c3c3c3c3-0000-4000-8000-000000000003";
pub const CLUB: &str = "5a175a175a175a175a175a175a175a175a175a175a175a175a175a175a175a17";
/// What the fake keeps as the key to a state it does not have.
const VAULT: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// 2026-10-13 12:00 UTC, in milliseconds: what the fake sends with is counted on from here.
const T0: u64 = 1_791_892_800_000;
/// The link the fake shows, and the one after it expires.
pub const LINK: &str = "sgnl://linkdevice?uuid=fake-1&pub_key=BfakeKey1";
pub const LINK_AGAIN: &str = "sgnl://linkdevice?uuid=fake-2&pub_key=BfakeKey2";

fn who(word: &str) -> Option<(&'static str, &'static str)> {
    match word {
        "grace" => Some((GRACE, "Grace Ombra")),
        "alan" => Some((ALAN, "Alan Ferro")),
        _ => None,
    }
}

fn record(line: &str) {
    let path = protocol::support_dir().join("signal.calls");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

pub struct Fake {
    events: Sender<Event>,
    heard: Mutex<Option<Receiver<Event>>>,
    closed: Arc<AtomicBool>,
    refuse: Arc<Mutex<Option<String>>>,
    clock: AtomicU64,
    control: PathBuf,
}

impl Fake {
    pub fn open(_saved: &Saved) -> Fake {
        let (events, heard) = channel();
        // What the phone says once a device connects: its contacts, its groups, its settings.
        let grace = Contact { id: GRACE.into(), name: "Grace Ombra".into(), phone: "+15550100002".into() };
        let alan = Contact { id: ALAN.into(), name: "Alan Ferro".into(), phone: String::new() };
        let _ = events.send(Event::Contacts(vec![grace, alan]));
        let club = GroupInfo { id: CLUB.into(), title: "Bridge club".into(), description: "Tuesdays, 7 pm".into(), members: 3 };
        let _ = events.send(Event::Groups(vec![club]));
        let _ = events.send(Event::Settings { read_receipts: true, typing: true });
        let _ = events.send(Event::Caught);
        let fake = Fake {
            events,
            heard: Mutex::new(Some(heard)),
            closed: Arc::default(),
            refuse: Arc::default(),
            clock: AtomicU64::new(T0 + 1_000_000),
            control: script::control("signal.ctl"),
        };
        fake.listen();
        fake
    }

    /// Reads the test's lines for as long as this wire lives; closing the wire ends it.
    fn listen(&self) {
        let (events, closed, refuse, control) = (self.events.clone(), self.closed.clone(), self.refuse.clone(), self.control.clone());
        let _ = std::thread::Builder::new().name("signal-fake".into()).spawn(move || {
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
    }
}

/// One line from the test: what Signal then says unasked.
fn steer(line: &str) -> Vec<Event> {
    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or("");
    let mut next = || words.next().unwrap_or("");
    let direct = |id: &str| Peer::Direct(id.to_owned());
    let club = || Peer::Group(CLUB.to_owned());
    let rest = |line: &str, skip: usize| line.split_whitespace().skip(skip).collect::<Vec<_>>().join(" ");
    match command {
        "incoming" | "group" | "mention" | "photo" | "quote" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let mut text = Text { author: author.into(), name: name.into(), sent, ..Text::default() };
            match command {
                "photo" => text.unsupported = Some("Photo".into()),
                "quote" => {
                    let quoted = next().parse().unwrap_or(0);
                    text.quote = Some(Quote { sent: quoted, author: ME.into(), text: String::new() });
                    text.text = rest(line, 4);
                }
                "mention" => {
                    text.text = format!("@Ada Quill {}", rest(line, 3));
                    text.mentions_me = true;
                }
                _ => text.text = rest(line, 3),
            }
            let peer = if matches!(command, "group" | "mention") { club() } else { direct(author) };
            vec![Event::Message { peer, text, outgoing: false }]
        }
        "mine" | "note" => {
            let sent = next().parse().unwrap_or(T0);
            let text = Text { author: ME.into(), name: "Ada Quill".into(), sent, text: rest(line, 2), ..Text::default() };
            let peer = direct(if command == "note" { ME } else { GRACE });
            vec![Event::Message { peer, text, outgoing: true }]
        }
        "edit" | "delete" => {
            let Some((author, _)) = who(next()) else {
                return Vec::new();
            };
            let target = next().parse().unwrap_or(0);
            match command {
                "edit" => vec![Event::Edited { peer: direct(author), author: author.into(), target, text: rest(line, 3) }],
                _ => vec![Event::Deleted { peer: direct(author), author: author.into(), target }],
            }
        }
        "react" | "unreact" => {
            let Some((by, _)) = who(next()) else {
                return Vec::new();
            };
            let author = match next() {
                "me" => ME,
                other => who(other).map_or(ME, |(id, _)| id),
            };
            let target = next().parse().unwrap_or(0);
            let emoji = next().to_owned();
            vec![Event::Reacted { peer: direct(by), by: by.into(), author: author.into(), target, emoji, removed: command == "unreact" }]
        }
        "read" => {
            let Some((author, _)) = who(next()) else {
                return Vec::new();
            };
            vec![Event::ReadElsewhere(vec![(author.into(), next().parse().unwrap_or(0))])]
        }
        "seen" | "delivered" => {
            let Some((by, _)) = who(next()) else {
                return Vec::new();
            };
            vec![Event::Receipt { by: by.into(), sent: vec![next().parse().unwrap_or(0)], read: command == "seen" }]
        }
        "typing" | "typing-stop" => {
            let Some((by, name)) = who(next()) else {
                return Vec::new();
            };
            vec![Event::Typing { peer: direct(by), by: by.into(), name: name.into(), started: command == "typing" }]
        }
        "drop" => vec![Event::Closed("the fake dropped the connection".into())],
        "unlinked" => vec![Event::Ended("This computer was removed from your Signal account.".into())],
        _ => Vec::new(),
    }
}

fn describe(to: &Peer) -> String {
    to.conversation()
}

impl Wire for Fake {
    fn events(&self) -> Option<Receiver<Event>> {
        self.heard.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }

    fn me(&self) -> (String, String) {
        (ME.to_owned(), "Ada Quill".to_owned())
    }

    fn send(&self, to: &Peer, body: Body) -> Result<u64, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let line = match &body {
            Body::Text { text, quote: Some(quote) } => format!("send {} text {text} (answering {}-{})", describe(to), quote.sent, quote.author),
            Body::Text { text, quote: None } => format!("send {} text {text}", describe(to)),
            Body::React { author, target, emoji, remove } => format!("send {} react {target}-{author} {emoji} remove={remove}", describe(to)),
            Body::Edit { target, text } => format!("send {} edit {target} {text}", describe(to)),
            Body::Delete { target } => format!("send {} delete {target}", describe(to)),
        };
        record(&line);
        if let Some(words) = self.refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        Ok(self.clock.fetch_add(1_000, Ordering::Relaxed))
    }

    fn typing(&self, to: &Peer, started: bool) -> Result<(), Fault> {
        record(&format!("typing {} {}", describe(to), if started { "started" } else { "stopped" }));
        Ok(())
    }

    fn read(&self, read: &[(String, u64)], receipts: bool) -> Result<(), Fault> {
        let read: Vec<String> = read.iter().map(|(author, sent)| format!("{sent}-{author}")).collect();
        record(&format!("read receipts={receipts} {}", read.join(",")));
        Ok(())
    }

    fn unlink(&self) -> Result<(), Fault> {
        record("unlink");
        Ok(())
    }

    fn close(&self) {
        // The reader sees it within 20 ms and stops.
        self.closed.store(true, Ordering::Relaxed);
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.close();
    }
}

/// Links when the test says the phone scanned the code.
pub struct FakeLinker;

impl Linker for FakeLinker {
    fn link(self: Box<Self>, device: &str, show: &dyn Fn(String), cancelled: Arc<AtomicBool>) -> Result<Linked, Fault> {
        record(&format!("link {device}"));
        let control = script::control("signal.signin.ctl");
        show(LINK.to_owned());
        let mut outcome = None;
        script::follow(&control, |line| {
            if cancelled.load(Ordering::Relaxed) {
                record("link cancelled");
                outcome = Some(Err(Fault::Net("cancelled".into())));
                return false;
            }
            match line.map(str::trim) {
                Some("scan") => {
                    let saved = Saved { aci: ME.into(), name: "Ada Quill".into(), vault: VAULT.into() };
                    outcome = Some(serde_json::to_string(&saved).map(|session| Linked { session }).map_err(|err| Fault::Net(err.to_string())));
                    false
                }
                Some("expire") => {
                    show(LINK_AGAIN.to_owned());
                    true
                }
                Some("fail") => {
                    outcome = Some(Err(Fault::Net("the fake could not reach Signal".into())));
                    false
                }
                _ => true,
            }
        });
        outcome.unwrap_or_else(|| Err(Fault::Net("the fake's control file could not be read".into())))
    }
}
