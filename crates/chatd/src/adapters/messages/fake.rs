//! A scripted Messages account, so the adapter can be driven through chatd's socket with no phone.
//!
//! `messages.signin.ctl` while linking: `confirm`, `expire`, `fail`.
//! `messages.ctl` once linked:
//! `incoming WHO TEXT…`, `group WHO TEXT…`, `photo WHO`, `caption WHO TEXT…`,
//! `file WHO NAME`, `video WHO`, `poster WHO`, `voice WHO`, `remote WHO`,
//! `react WHO TEXT`, `quote WHO ID TEXT…`, `typing WHO`, `typing-stop WHO`,
//! `archive WHO`, `spam WHO`, `again WHO`,
//! `refuse WORDS…`, `status CODE`, `hold`, `go`, `drop`, `unlinked`, `silent`, `answer`,
//! `revoke`, `revoke-offline`, `auth`.
//! `archive` files the chat as archived. `spam` files it as spam. `again` repeats the
//! chat with a new title and no status, which must keep the folder it already had.
//! `silent` makes the phone stop answering sends, history, and files. `answer` clears that
//! and tells the adapter the phone is back.
//! `hold` pauses a file upload until `go`, so a cancel can land while it is still unsent.
//! WHO is `grace` or `alan`. `status` reports that code on the last text this computer sent.

use super::{Drive, Event, Saved};
use crate::adapters::messages::bugle::{Chat, Mark, MediaPart, Note, Participant, Piece, MESSAGE_PAGE, PHONE_QUIET, UNPAIRED};
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

#[derive(Clone)]
struct SentCopy {
    conversation: String,
    text: String,
    relay_id: String,
    phone_id: String,
}

pub(super) fn record(line: &str) {
    let path = protocol::support_dir().join("messages.calls");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(format!("{line}\n").as_bytes());
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
    last: Arc<Mutex<Option<SentCopy>>>,
    /// A scripted upload waits while this is set (`hold` / `go` on `messages.ctl`).
    hold: Arc<AtomicBool>,
}

impl Fake {
    pub fn open(_saved: &Saved) -> Fake {
        let revoked = protocol::support_dir().join("messages.revoked");
        if revoked.is_file() {
            let _ = std::fs::remove_file(&revoked);
            record("connect");
            let (events, heard) = channel();
            let _ = events.send(Event::Ended(UNPAIRED.to_owned()));
            return Fake {
                events,
                heard: Mutex::new(Some(heard)),
                closed: Arc::new(AtomicBool::new(true)),
                refuse: Arc::default(),
                quiet: Arc::default(),
                sent: AtomicU32::new(0),
                last: Arc::default(),
                hold: Arc::default(),
            };
        }
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
                status: 1,
                read_only: false,
                verified: false,
                participants: vec![
                    Participant {
                        id: ME.into(),
                        name: "Ada Quill".into(),
                        number: "+15550100001".into(),
                        is_me: true,
                    },
                    Participant {
                        id: GRACE_PARTICIPANT.into(),
                        name: "Grace Ombra".into(),
                        number: "+15550100002".into(),
                        is_me: false,
                    },
                ],
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
                status: 1,
                read_only: false,
                verified: false,
                participants: vec![
                    Participant {
                        id: ME.into(),
                        name: "Ada Quill".into(),
                        number: "+15550100001".into(),
                        is_me: true,
                    },
                    Participant {
                        id: GRACE_PARTICIPANT.into(),
                        name: "Grace Ombra".into(),
                        number: "+15550100002".into(),
                        is_me: false,
                    },
                    Participant {
                        id: "participant-alan".into(),
                        name: "Alan Ferro".into(),
                        number: "+15550100003".into(),
                        is_me: false,
                    },
                ],
            },
        ]));
        let fake = Fake {
            events,
            heard: Mutex::new(Some(heard)),
            closed: Arc::default(),
            refuse: Arc::default(),
            quiet: Arc::default(),
            sent: AtomicU32::new(0),
            last: Arc::default(),
            hold: Arc::default(),
        };
        let (events, closed, refuse, quiet, last, hold) = (
            fake.events.clone(),
            fake.closed.clone(),
            fake.refuse.clone(),
            fake.quiet.clone(),
            fake.last.clone(),
            fake.hold.clone(),
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
                    if let Some(code) = line.strip_prefix("status ") {
                        let code = code.split_whitespace().next().unwrap_or("").parse::<u64>().unwrap_or(0);
                        if let Some(sent) = last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone() {
                            let mut spoken = note(
                                ME,
                                "Ada Quill",
                                &sent.conversation,
                                &sent.phone_id,
                                &sent.text,
                                false,
                                Vec::new(),
                                Vec::new(),
                            );
                            spoken.outgoing = true;
                            spoken.tmp = sent.relay_id;
                            spoken.status = code;
                            // The phone's clock, not the fixture stamp: a text still sending must
                            // not look an hour old and be failed for silence.
                            spoken.sent = crate::hub::now();
                            let _ = events.send(Event::Message(spoken));
                        }
                        return true;
                    }
                    if line == "hold" {
                        hold.store(true, Ordering::Release);
                        record("hold");
                        return true;
                    }
                    if line == "go" {
                        hold.store(false, Ordering::Release);
                        record("go");
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

    fn remember(&self, conversation: &str, text: &str) -> String {
        let n = self.sent.fetch_add(1, Ordering::Relaxed) + 1;
        let relay_id = format!("relay-{n}");
        let phone_id = format!("phone-{n}");
        *self.last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(SentCopy {
            conversation: conversation.to_owned(),
            text: text.to_owned(),
            relay_id: relay_id.clone(),
            phone_id,
        });
        relay_id
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
            // The 90th-newest line. A photo here is older than the first page, so a shared
            // search that only saw that page would miss it.
            if n == 11 {
                line.pieces = vec![photo_piece(true)];
            }
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
        tmp: String::new(),
        detail: String::new(),
        wire: 0,
        phones: None,
        reply: String::new(),
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

/// A video, a video with a still in the inline bytes, or a voice note.
fn media_note(
    who_word: &str,
    text: &str,
    filename: &str,
    mime: &str,
    inline: Vec<u8>,
    media_id: String,
) -> Vec<Event> {
    let Some((author, name)) = who(who_word) else {
        return Vec::new();
    };
    let size = if media_id.is_empty() {
        inline.len() as u64
    } else {
        120_000
    };
    let piece = Piece {
        name: filename.into(),
        mime: mime.into(),
        media_id,
        inline,
        size,
        ..Piece::default()
    };
    vec![Event::Message(note(
        author,
        name,
        author,
        &format!("m-{author}-{text}"),
        text,
        true,
        vec![piece],
        Vec::new(),
    ))]
}

fn png() -> Vec<u8> {
    const BYTES: &str = "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000c4944415408d763f8cfc0000000020001e221bc330000000049454e44ae426082";
    (0..BYTES.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&BYTES[at..at + 2], 16).unwrap())
        .collect()
}

fn listed(id: &str, title: &str, status: u64) -> Chat {
    Chat {
        id: id.into(),
        title: title.into(),
        group: false,
        unread: false,
        mine: ME.into(),
        peer: if id == GRACE {
            GRACE_PARTICIPANT.into()
        } else {
            String::new()
        },
        carrier: 0,
        updated: 0,
        phones: false,
        status,
        read_only: false,
        verified: false,
        participants: Vec::new(),
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
        "video" => media_note(
            words.next().unwrap_or(""),
            "clip",
            "clip.mp4",
            "video/mp4",
            super::clip_bytes(),
            String::new(),
        ),
        "poster" => media_note(
            words.next().unwrap_or(""),
            "poster",
            "clip.mp4",
            "video/mp4",
            png(),
            "clip-1".into(),
        ),
        "voice" => media_note(
            words.next().unwrap_or(""),
            "voice",
            "note.amr",
            "audio/amr",
            super::voice_bytes(),
            String::new(),
        ),
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
        "quote" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let original = words.next().unwrap_or("").to_owned();
            let text = rest(3);
            let mut sent = note(
                author,
                name,
                author,
                &format!("m-{author}-quote"),
                &text,
                false,
                Vec::new(),
                Vec::new(),
            );
            sent.reply = original;
            vec![Event::Message(sent)]
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
        "typing-raw" => {
            let conv = words.next().unwrap_or("");
            let raw = words.next().unwrap_or("");
            if conv.is_empty() || raw.is_empty() {
                return Vec::new();
            }
            vec![Event::Typing {
                conversation: conv.into(),
                by: raw.into(),
                name: raw.into(),
                started: true,
            }]
        }
        "drop" => vec![Event::Closed("the connection dropped".into())],
        "unlinked" => vec![Event::Ended("This device was removed in Messages.".into())],
        "revoke" => vec![Event::Ended(UNPAIRED.to_owned())],
        "revoke-offline" => {
            let path = protocol::support_dir().join("messages.revoked");
            let _ = std::fs::write(path, b"1");
            vec![Event::Closed("the connection dropped".into())]
        }
        "auth" => vec![Event::Closed("Messages refused the request (403).".into())],
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
                status: 0,
                read_only: false,
                verified: false,
                participants: Vec::new(),
            }])]
        }
        "archive" | "spam" | "again" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let (title, status) = match command {
                "archive" => (name.to_owned(), 2),
                "spam" => (name.to_owned(), 5),
                _ => (format!("{name} still"), 0),
            };
            vec![Event::Chats(vec![listed(author, &title, status)])]
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
        "erase" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let id = words.next().unwrap_or("");
            let mut sent = note(author, name, author, id, "", false, Vec::new(), Vec::new());
            sent.status = 300;
            vec![Event::Message(sent)]
        }
        "joined" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let mut sent = note(author, name, author, "m-joined", "", false, Vec::new(), Vec::new());
            sent.status = 200;
            vec![Event::Message(sent)]
        }
        "gone" => {
            let Some((author, name)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
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
                carrier: 0,
                updated: 0,
                phones: false,
                status: 3,
                read_only: false,
                verified: false,
                participants: Vec::new(),
            }])]
        }
        "hold" | "read" => {
            let Some((author, _)) = who(words.next().unwrap_or("")) else {
                return Vec::new();
            };
            let unread = command == "hold";
            vec![Event::Chats(vec![Chat {
                id: author.into(),
                title: if unread { "Held".into() } else { "Read".into() },
                group: false,
                unread,
                mine: ME.into(),
                peer: if author == GRACE {
                    GRACE_PARTICIPANT.into()
                } else {
                    String::new()
                },
                carrier: 0,
                updated: 0,
                phones: false,
                status: 1,
                read_only: false,
                verified: false,
                participants: Vec::new(),
            }])]
        }
        "noreply" => {
            let id = words.next().unwrap_or("phone-bank");
            let sender = words.next().unwrap_or("BANKOTP");
            vec![Event::Chats(vec![Chat {
                id: id.into(),
                title: sender.into(),
                mine: ME.into(),
                peer: format!("participant-{id}"),
                status: 1,
                participants: vec![
                    Participant {
                        id: ME.into(),
                        name: "Ada Quill".into(),
                        number: "+15550100001".into(),
                        is_me: true,
                    },
                    Participant {
                        id: format!("participant-{id}"),
                        name: sender.into(),
                        number: sender.into(),
                        is_me: false,
                    },
                ],
                ..Chat::default()
            }])]
        }
        "shortcode" => {
            let id = words.next().unwrap_or("phone-short");
            let code = words.next().unwrap_or("89203");
            vec![Event::Chats(vec![Chat {
                id: id.into(),
                title: code.into(),
                mine: ME.into(),
                peer: format!("participant-{id}"),
                status: 1,
                participants: vec![
                    Participant {
                        id: ME.into(),
                        name: "Ada Quill".into(),
                        number: "+15550100001".into(),
                        is_me: true,
                    },
                    Participant {
                        id: format!("participant-{id}"),
                        name: code.into(),
                        number: code.into(),
                        is_me: false,
                    },
                ],
                ..Chat::default()
            }])]
        }
        "verified" => {
            let id = words.next().unwrap_or("phone-biz");
            let name = rest(2);
            let title = if name.is_empty() { "Acme Bank".into() } else { name };
            vec![Event::Chats(vec![Chat {
                id: id.into(),
                title: title.clone(),
                mine: ME.into(),
                peer: format!("participant-{id}"),
                carrier: 3,
                status: 1,
                verified: true,
                participants: vec![
                    Participant {
                        id: ME.into(),
                        name: "Ada Quill".into(),
                        number: "+15550100001".into(),
                        is_me: true,
                    },
                    Participant {
                        id: format!("participant-{id}"),
                        name: title,
                        number: "+18005550199".into(),
                        is_me: false,
                    },
                ],
                ..Chat::default()
            }])]
        }
        "emergency" => {
            let id = words.next().unwrap_or("phone-911");
            let number = words.next().unwrap_or("911");
            vec![Event::Chats(vec![Chat {
                id: id.into(),
                title: number.into(),
                mine: ME.into(),
                peer: format!("participant-{id}"),
                status: 1,
                participants: vec![
                    Participant {
                        id: ME.into(),
                        name: "Ada Quill".into(),
                        number: "+15550100001".into(),
                        is_me: true,
                    },
                    Participant {
                        id: format!("participant-{id}"),
                        name: number.into(),
                        number: number.into(),
                        is_me: false,
                    },
                ],
                ..Chat::default()
            }])]
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
    fn send(
        &self,
        conversation: &str,
        text: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        self.unanswered()?;
        if let Some(words) = self.refusal() {
            return Err(words);
        }
        let relay_id = self.remember(conversation, text);
        noted(&relay_id);
        record(&format!("send {conversation} {text}"));
        if let Some(id) = reply.filter(|id| !id.is_empty()) {
            record(&format!("reply {conversation} {id}"));
        }
        Ok(relay_id)
    }
    fn upload_media(&self, _packed: &[u8], _mime: &str) -> Result<String, String> {
        while self.hold.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.unanswered()?;
        if let Some(words) = self.refusal() {
            return Err(words);
        }
        Ok("media".into())
    }
    fn send_media(
        &self,
        conversation: &str,
        caption: &str,
        parts: &[MediaPart<'_>],
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        self.unanswered()?;
        if let Some(words) = self.refusal() {
            return Err(words);
        }
        let relay_id = self.remember(conversation, caption);
        noted(&relay_id);
        for part in parts {
            record(&format!(
                "file {conversation} {} {} {caption}",
                part.name, part.size
            ));
        }
        if let Some(id) = reply.filter(|id| !id.is_empty()) {
            record(&format!("reply {conversation} {id}"));
        }
        Ok(relay_id)
    }
    fn resend(&self, message: &str) -> Result<(), String> {
        self.unanswered()?;
        if let Some(words) = self.refusal() {
            return Err(words);
        }
        record(&format!("resend {message}"));
        Ok(())
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
    fn delete_message(&self, message: &str) -> Result<(), String> {
        record(&format!("delete {message}"));
        Ok(())
    }
    fn delete_conversation(&self, conversation: &str) -> Result<(), String> {
        record(&format!("delete-chat {conversation}"));
        Ok(())
    }
    fn unpair(&self) -> Result<(), String> {
        record("unpair");
        Ok(())
    }
    fn block(&self, conversation: &str) -> Result<(), String> {
        self.unanswered()?;
        record(&format!("block {conversation}"));
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
