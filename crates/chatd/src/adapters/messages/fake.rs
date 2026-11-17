//! A scripted Messages account, so the adapter can be driven through chatd's socket with no phone.
//!
//! `messages.signin.ctl` while linking: `confirm`, `expire`, `fail`.
//! `messages.ctl` once linked:
//! `incoming WHO TEXT…`, `group WHO TEXT…`, `photo WHO`, `caption WHO TEXT…`,
//! `file WHO NAME`, `remote WHO`, `react WHO TEXT`, `typing WHO`, `typing-stop WHO`,
//! `refuse WORDS…`, `drop`, `unlinked`, `silent`, `answer`.
//! `silent` makes the phone stop answering sends, history, and files. `answer` clears that
//! and tells the adapter the phone is back.
//! WHO is `grace` or `alan`.

use super::{Drive, Event, Saved};
use crate::adapters::messages::bugle::{Chat, Mark, Note, Piece, MESSAGE_PAGE, PHONE_QUIET};
use crate::adapters::script;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const ME: &str = "phone-ada";
pub const GRACE: &str = "phone-grace";
pub const ALAN: &str = "phone-alan";
pub const CLUB: &str = "phone-club";
/// Grace as a participant of her chat: whose picture the phone is asked for.
pub const GRACE_PARTICIPANT: &str = "participant-grace";
pub const EMOJI: &str = "🐢";
pub const EMOJI_AGAIN: &str = "🐈";

fn record(line: &str) {
    let path = protocol::support_dir().join("messages.calls");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

pub struct Fake {
    events: Sender<Event>,
    heard: Mutex<Option<Receiver<Event>>>,
    closed: Arc<AtomicBool>,
    refuse: Arc<Mutex<Option<String>>>,
    /// Set by `silent`, cleared by `answer`. Requests fail at once and are not recorded.
    quiet: Arc<AtomicBool>,
    sent: AtomicU32,
}

impl Fake {
    pub fn open(_saved: &Saved) -> Fake {
        let (events, heard) = channel();
        let _ = events.send(Event::Ready);
        let _ = events.send(Event::Chats(vec![
            Chat {
                id: GRACE.into(),
                title: "Grace Ombra".into(),
                group: false,
                unread: false,
                mine: ME.into(),
                peer: GRACE_PARTICIPANT.into(),
                carrier: 0,
                updated: 0,
                phones: false,
            },
            Chat {
                id: CLUB.into(),
                title: "Bridge club".into(),
                group: true,
                unread: true,
                mine: ME.into(),
                peer: String::new(),
                carrier: 0,
                updated: 0,
                phones: false,
            },
        ]));
        let fake = Fake {
            events,
            heard: Mutex::new(Some(heard)),
            closed: Arc::default(),
            refuse: Arc::default(),
            quiet: Arc::default(),
            sent: AtomicU32::new(0),
        };
        let (events, closed, refuse, quiet) = (
            fake.events.clone(),
            fake.closed.clone(),
            fake.refuse.clone(),
            fake.quiet.clone(),
        );
        let control = script::control("messages.ctl");
        let _ = std::thread::Builder::new()
            .name("messages-fake".into())
            .spawn(move || {
                script::follow(&control, |line| {
                    if closed.load(Ordering::Relaxed) {
                        return false;
                    }
                    let Some(line) = line.map(str::trim) else {
                        return true;
                    };
                    if let Some(words) = line.strip_prefix("refuse ") {
                        *refuse
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            Some(words.to_owned());
                        return true;
                    }
                    if line == "silent" {
                        quiet.store(true, Ordering::Relaxed);
                        // Recorded after the flag, so a test can wait until the phone is quiet.
                        // The control file is marked taken before this line runs.
                        record("silent");
                        return true;
                    }
                    if line == "answer" {
                        quiet.store(false, Ordering::Relaxed);
                        record("answer");
                        let _ = events.send(Event::PhoneBack);
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

    /// The phone did not answer. The call is not recorded, so a test can see it was not asked.
    fn unanswered(&self) -> Result<(), String> {
        if self.quiet.load(Ordering::Relaxed) {
            Err(PHONE_QUIET.to_owned())
        } else {
            Ok(())
        }
    }

    fn refusal(&self) -> Option<String> {
        self.refuse
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// `TINYCLIENT_MESSAGES_MANY=100` is one conversation of 100 messages, newest page first.
/// A cursor id `m-NNNN` is exclusive: the next page ends at the message before it.
fn scripted_page(conversation: &str, before: Option<&str>) -> Vec<Note> {
    if conversation != GRACE || std::env::var("TINYCLIENT_MESSAGES_MANY").ok().as_deref() != Some("100") {
        return Vec::new();
    }
    let end = match before {
        None => 100,
        Some(id) => {
            let Some(n) = id.strip_prefix("m-").and_then(|n| n.parse::<i64>().ok()) else {
                return Vec::new();
            };
            n - 1
        }
    };
    if end < 1 {
        return Vec::new();
    }
    let start = (end - MESSAGE_PAGE as i64 + 1).max(1);
    (start..=end)
        .map(|n| {
            let mut line = note(GRACE, "Grace Ombra", GRACE, &format!("m-{n:04}"), &format!("line {n}"), false, Vec::new(), Vec::new());
            line.sent = 1_700_000_000 + n;
            line
        })
        .collect()
}

fn note(
    author: &str,
    name: &str,
    conversation: &str,
    id: &str,
    text: &str,
    file: bool,
    pieces: Vec<Piece>,
    reactions: Vec<Mark>,
) -> Note {
    Note {
        id: id.into(),
        conversation: conversation.into(),
        author: author.into(),
        name: name.into(),
        sent: 1_791_892_800,
        text: text.into(),
        outgoing: false,
        file,
        pieces,
        reactions,
        status: 0,
        wire: 0,
        phones: None,
    }
}

/// A 1×1 PNG. `inline` is false when the shell has to ask for the bytes.
fn photo_piece(inline: bool) -> Piece {
    Piece {
        name: "photo.png".into(),
        mime: "image/png".into(),
        media_id: if inline {
            String::new()
        } else {
            "fake-photo".into()
        },
        inline: if inline { png() } else { Vec::new() },
        size: png().len() as u64,
        width: 1,
        height: 1,
        ..Piece::default()
    }
}

fn png() -> Vec<u8> {
    const BYTES: &str = "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000c4944415408d763f8cfc0000000020001e221bc330000000049454e44ae426082";
    (0..BYTES.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&BYTES[at..at + 2], 16).unwrap())
        .collect()
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
    let rest = |skip| {
        line.split_whitespace()
            .skip(skip)
            .collect::<Vec<_>>()
            .join(" ")
    };
    match command {
        "incoming" | "group" | "photo" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let group = command == "group";
            let conversation = if group { CLUB } else { author };
            let (text, file, pieces) = match command {
                "photo" => (String::new(), true, vec![photo_piece(true)]),
                _ => (rest(2), false, Vec::new()),
            };
            vec![Event::Message(note(
                author,
                name,
                conversation,
                &format!("m-{author}-{}", text.len().max(1)),
                &text,
                file,
                pieces,
                Vec::new(),
            ))]
        }
        "caption" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let text = rest(2);
            vec![Event::Message(note(
                author,
                name,
                author,
                &format!("m-{author}-cap"),
                &text,
                false,
                vec![photo_piece(true)],
                Vec::new(),
            ))]
        }
        "file" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let filename = words.next().unwrap_or("report.pdf");
            let piece = Piece {
                name: filename.into(),
                mime: "application/pdf".into(),
                ..Piece::default()
            };
            vec![Event::Message(note(
                author,
                name,
                author,
                &format!("m-{author}-file"),
                filename,
                true,
                vec![piece],
                Vec::new(),
            ))]
        }
        "remote" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            vec![Event::Message(note(
                author,
                name,
                author,
                &format!("m-{author}-remote"),
                "",
                true,
                vec![photo_piece(false)],
                Vec::new(),
            ))]
        }
        "react" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let text = rest(2);
            let mark = Mark {
                emoji: "👍".into(),
                who: vec![author.into()],
            };
            vec![Event::Message(note(
                author,
                name,
                author,
                &format!("m-{author}-react"),
                &text,
                false,
                Vec::new(),
                vec![mark],
            ))]
        }
        "typing" | "typing-stop" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            vec![Event::Typing {
                conversation: author.into(),
                by: author.into(),
                name: name.into(),
                started: command == "typing",
            }]
        }
        "drop" => vec![Event::Closed("the connection dropped".into())],
        "unlinked" => vec![Event::Ended("This device was removed in Messages.".into())],
        "sms" | "rcs" | "sealed" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let (carrier, phones) = match command {
                "sms" => (1, false),
                "sealed" => (2, true),
                _ => (2, false),
            };
            vec![Event::Chats(vec![Chat {
                id: author.into(),
                title: name.into(),
                group: false,
                unread: false,
                mine: ME.into(),
                peer: if author == GRACE {
                    GRACE_PARTICIPANT.into()
                } else {
                    String::new()
                },
                carrier,
                updated: 0,
                phones,
            }])]
        }
        "tick" => {
            let Some((author, _)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let mut sent = note(ME, "Ada Quill", author, "m-mine-tick", "sent", false, Vec::new(), Vec::new());
            sent.outgoing = true;
            sent.status = 11;
            vec![Event::Message(sent)]
        }
        "mms" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let mut sent = note(
                author,
                name,
                author,
                &format!("m-{author}-mms"),
                &rest(2),
                false,
                Vec::new(),
                Vec::new(),
            );
            sent.wire = 2;
            vec![Event::Message(sent)]
        }
        _ => Vec::new(),
    }
}

impl Drive for Fake {
    fn events(&self) -> Option<Receiver<Event>> {
        self.heard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
    fn me(&self) -> (String, String) {
        (ME.into(), "Ada Quill".into())
    }
    fn refresh(&self) -> Result<(), String> {
        record("list");
        Ok(())
    }
    fn history(&self, conversation: &str, before: Option<(&str, i64)>) -> Result<Vec<Note>, String> {
        self.unanswered()?;
        match before {
            Some((id, _)) => record(&format!("history {conversation} {id}")),
            None => record(&format!("history {conversation}")),
        }
        Ok(scripted_page(conversation, before.map(|(id, _)| id)))
    }
    fn older_chats(&self) -> Result<Vec<Chat>, String> {
        Ok(Vec::new())
    }
    fn inbox_continues(&self) -> bool {
        false
    }
    fn send(&self, conversation: &str, text: &str) -> Result<String, String> {
        self.unanswered()?;
        if let Some(words) = self.refusal() {
            return Err(words);
        }
        record(&format!("send {conversation} {text}"));
        let n = self.sent.fetch_add(1, Ordering::Relaxed);
        Ok(format!("sent-{conversation}-{n}"))
    }
    fn send_file(
        &self,
        conversation: &str,
        caption: &str,
        path: &str,
        name: &str,
    ) -> Result<String, String> {
        self.unanswered()?;
        if let Some(words) = self.refusal() {
            return Err(words);
        }
        let bytes = std::fs::read(path).map_err(|_| format!("Messages couldn't read {name}."))?;
        record(&format!(
            "file {conversation} {name} {} {caption}",
            bytes.len()
        ));
        let n = self.sent.fetch_add(1, Ordering::Relaxed);
        Ok(format!("sent-file-{conversation}-{n}"))
    }
    fn react(&self, message: &str, emoji: &str, on: bool) -> Result<(), String> {
        if let Some(words) = self
            .refuse
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            return Err(words);
        }
        record(&format!("react {message} {emoji} {}", u8::from(on)));
        Ok(())
    }
    fn fetch(
        &self,
        media_id: &str,
        _key: &[u8],
        message: &str,
        _action: &str,
    ) -> Result<Vec<u8>, String> {
        self.unanswered()?;
        record(&format!("fetch {message} {media_id}"));
        Ok(png())
    }
    fn thumbnails(&self, participants: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.unanswered()?;
        record(&format!("thumbnails {}", participants.join(" ")));
        Ok(participants
            .iter()
            .filter(|participant| *participant == GRACE_PARTICIPANT)
            .map(|participant| (participant.clone(), png()))
            .collect())
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

/// Links when the test says the phone confirmed the matching emoji.
pub struct FakeLinker;

impl FakeLinker {
    pub fn link(show: &dyn Fn(String), cancelled: &AtomicBool) -> Result<String, String> {
        let control = script::control("messages.signin.ctl");
        show(EMOJI.to_owned());
        let mut outcome = None;
        script::follow(&control, |line| {
            if cancelled.load(Ordering::Relaxed) {
                outcome = Some(Err("cancelled".into()));
                return false;
            }
            match line.map(str::trim) {
                Some("confirm") => {
                    let session = serde_json::json!({ "account": ME, "name": "Ada Quill" });
                    outcome = Some(Ok(session.to_string()));
                    false
                }
                Some("expire") => {
                    show(EMOJI_AGAIN.to_owned());
                    true
                }
                Some("fail") => {
                    outcome = Some(Err("Messages could not pair.".into()));
                    false
                }
                _ => true,
            }
        });
        record("pairing stopped");
        outcome.unwrap_or_else(|| Err("the pairing was interrupted".into()))
    }
}
