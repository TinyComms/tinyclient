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
//! | `group-title TITLE…` | the group was renamed to TITLE |
//! | `mention WHO SENT TEXT…` | the same, in the group, naming us |
//! | `photo WHO SENT` | WHO sent a photo with no caption |
//! | `payment WHO SENT NOTE…` | WHO sent a payment notification; its note, when words follow |
//! | `giftbadge WHO SENT` | WHO sent a gift badge, redeemed on the phone |
//! | `viewonce WHO SENT` | WHO sent a view-once photo, which opens on the phone |
//! | `viewonce-video WHO SENT` | WHO sent a view-once video, which opens on the phone |
//! | `longtext WHO SENT TEXT…` | WHO sent text too long for one message: TEXT is the part in the body, the rest on the phone |
//! | `quote WHO SENT QUOTED TEXT…` | WHO answered our message sent at QUOTED |
//! | `mine SENT TEXT…` | we wrote TEXT to Grace from the phone |
//! | `note SENT TEXT…` | we wrote TEXT to Note to Self from the phone |
//! | `edit WHO TARGET TEXT…`, `delete WHO TARGET` | WHO changed or took back their message |
//! | `delete-for-me WHO SENT` | the phone deleted WHO's message, sent at SENT, for this device only |
//! | `delete-chat WHO` | the phone deleted the whole chat with WHO for this device only |
//! | `react WHO AUTHOR TARGET EMOJI`, `unreact …` | WHO reacted to AUTHOR's message |
//! | `poll WHO SENT QUESTION \| OPT…`, `pollmulti …` | WHO started a poll in the group; `pollmulti` allows more than one choice |
//! | `vote WHO AUTHOR TARGET INDEXES`, `unvote WHO AUTHOR TARGET` | WHO voted on AUTHOR's poll (indexes `0` or `0,1`), or took it back |
//! | `endpoll AUTHOR TARGET` | AUTHOR ended their poll |
//! | `read WHO SENT` | we read WHO's message on the phone |
//! | `seen WHO SENT`, `delivered WHO SENT`, `viewed WHO SENT` | WHO read our message, their phone has it, or they played it. SENT may list several times, separated by commas |
//! | `receipts off` | this account stops sharing read receipts |
//! | `fail WHO` | the next group send does not reach WHO |
//! | `typing WHO`, `typing-stop WHO` | WHO is typing to us, or stopped |
//! | `call WHO SENT`, `call-video WHO SENT` | WHO calls us (a voice offer, or a video one), call id SENT |
//! | `hangup SENT`, `busy SENT` | the call SENT ended, or we were busy on the other side |
//! | `hangup-accepted SENT` | the call SENT was answered on another of our devices |
//! | `refuse WORDS…` | the next send is refused, in those words |
//! | `lookup USERNAME ACI` | a later username search for USERNAME answers ACI |
//! | `drop`, `unlinked` | the connection ends; the phone removed this device |
//!
//! WHO is `grace` or `alan`. Every request it is sent is appended to `signal.calls`, one line
//! each, so a test can see what the network would have heard.
//!
//! The account: Ada Quill (us); Grace Ombra and Alan Ferro, both contacts; the group "Bridge
//! club" with all three of us in it.

use super::wire::{gift_badge_label, payment_label, view_once_label, Body, ChatFolder, ChatState, Contact, CONTINUES, Event, Fault, GroupInfo, Linked, Linker, MessageRequestAction, OwnCard, Peer, Quote, Text, Wire};
use super::Saved;
use crate::adapters::script;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const ME: &str = "a1a1a1a1-0000-4000-8000-000000000001";
pub const GRACE: &str = "b2b2b2b2-0000-4000-8000-000000000002";
pub const ALAN: &str = "c3c3c3c3-0000-4000-8000-000000000003";
pub const STRANGER: &str = "d4d4d4d4-0000-4000-8000-000000000004";
pub const CLUB: &str = "5a175a175a175a175a175a175a175a175a175a175a175a175a175a175a175a17";
/// What the fake keeps as the key to a state it does not have.
const VAULT: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// 2026-10-13 12:00 UTC, in milliseconds. A scripted line that names no time uses this, which is
/// outside the edit and delete-for-everyone windows. A send from the test uses the clock below.
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
    /// One person the next group send does not reach, named by a test.
    fail_one: Arc<Mutex<Option<String>>>,
    /// Taken by the adapter after that send.
    missed: Mutex<Vec<String>>,
    clock: AtomicU64,
    control: PathBuf,
    card: Arc<Mutex<OwnCard>>,
    /// Usernames a test has scripted, lower case, to an account id.
    lookups: Arc<Mutex<HashMap<String, String>>>,
}

impl Fake {
    pub fn open(_saved: &Saved) -> Fake {
        let (events, heard) = channel();
        // What the phone says once a device connects: its contacts, its groups, its settings.
        let grace = Contact { id: GRACE.into(), name: "Grace Ombra".into(), phone: "+15550100002".into() };
        let alan = Contact { id: ALAN.into(), name: "Alan Ferro".into(), phone: String::new() };
        let _ = events.send(Event::Contacts(vec![grace, alan]));
        let club = GroupInfo {
            id: CLUB.into(),
            title: "Bridge club".into(),
            description: "Tuesdays, 7 pm".into(),
            members: 3,
            member: true,
            member_ids: vec![ME.into(), GRACE.into(), ALAN.into()],
            timer_secs: 0,
        };
        let _ = events.send(Event::Groups(vec![club]));
        let _ = events.send(Event::Settings { read_receipts: true, typing: true });
        let _ = events.send(Event::Caught);
        let fake = Fake {
            events,
            heard: Mutex::new(Some(heard)),
            closed: Arc::default(),
            refuse: Arc::default(),
            fail_one: Arc::default(),
            missed: Mutex::default(),
            clock: AtomicU64::new(super::now_ms()),
            control: script::control("signal.ctl"),
            // A scripted card. The link is invented: nothing here reserves a username.
            card: Arc::new(Mutex::new(OwnCard {
                name: "Ada Quill".into(),
                about: "Keeps the score.".into(),
                emoji: "🌉".into(),
                username: "ada.12".into(),
                phone: "+15550100001".into(),
                link: "https://signal.me/#eu/fake-ada".into(),
            })),
            lookups: Arc::default(),
        };
        fake.listen();
        fake
    }

    /// Reads the test's lines for as long as this wire lives; closing the wire ends it.
    fn listen(&self) {
        let (events, closed, refuse, fail_one, control, lookups) = (self.events.clone(), self.closed.clone(), self.refuse.clone(), self.fail_one.clone(), self.control.clone(), self.lookups.clone());
        let _ = std::thread::Builder::new().name("signal-fake".into()).spawn(move || {
            script::follow(&control, |line| {
                if closed.load(Ordering::Relaxed) {
                    return false;
                }
                let Some(line) = line.map(str::trim) else {
                    return true;
                };
                if let Some(rest) = line.strip_prefix("lookup ") {
                    let mut parts = rest.split_whitespace();
                    let username = parts.next().unwrap_or("");
                    let aci = parts.next().unwrap_or("");
                    if !username.is_empty() && super::wire::is_aci(aci) {
                        lookups.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(username.to_ascii_lowercase(), aci.to_owned());
                    }
                    return true;
                }
                if let Some(words) = line.strip_prefix("refuse ") {
                    *refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(words.to_owned());
                    return true;
                }
                if let Some(who_word) = line.strip_prefix("fail ") {
                    *fail_one.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = who(who_word.trim()).map(|(id, _)| id.to_owned());
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
        "timer" => {
            let Some((author, _)) = who(next()) else {
                return Vec::new();
            };
            let seconds = next().parse().unwrap_or(0);
            vec![Event::TimerChanged {
                peer: direct(author),
                seconds,
                version: u32::from(seconds > 0),
            }]
        }
        "expiring" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let secs = next().parse().unwrap_or(0);
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                text: rest(line, 4),
                expire_timer: secs,
                expire_timer_version: u32::from(secs > 0),
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
        }
        "incoming" | "group" | "mention" | "photo" | "quote" | "payment" | "giftbadge" | "viewonce" | "viewonce-video" | "longtext" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let mut text = Text { author: author.into(), name: name.into(), sent, ..Text::default() };
            match command {
                "photo" => text.unsupported = Some("Photo".into()),
                "payment" => {
                    let note = rest(line, 3);
                    text.unsupported = Some(payment_label((!note.is_empty()).then_some(note.as_str())));
                }
                "giftbadge" => text.unsupported = Some(gift_badge_label().to_owned()),
                "viewonce" => text.unsupported = Some(view_once_label(Some("image/jpeg")).to_owned()),
                "viewonce-video" => text.unsupported = Some(view_once_label(Some("video/mp4")).to_owned()),
                "quote" => {
                    let quoted = next().parse().unwrap_or(0);
                    text.quote = Some(Quote { sent: quoted, author: ME.into(), text: String::new(), spans: Vec::new() });
                    text.text = rest(line, 4);
                }
                "mention" => {
                    text.text = format!("@Ada Quill {}", rest(line, 3));
                    text.mentions_me = true;
                }
                "longtext" => {
                    let body = rest(line, 3);
                    text.text = format!("{body}{CONTINUES}");
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
        "edit" | "delete" | "delete-for-me" => {
            let Some((author, _)) = who(next()) else {
                return Vec::new();
            };
            let target = next().parse().unwrap_or(0);
            let peer = direct(author);
            match command {
                "edit" => vec![Event::Edited { peer, author: author.into(), target, at: T0 + 60_000, text: rest(line, 3), spans: Vec::new() }],
                "delete-for-me" => vec![Event::DeletedForMe { peer, author: author.into(), target }],
                _ => vec![Event::Deleted { peer, author: author.into(), target }],
            }
        }
        "delete-chat" => {
            let Some((author, _)) = who(next()) else {
                return Vec::new();
            };
            vec![Event::Forgot(direct(author))]
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
        "poll" | "pollmulti" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let body = rest(line, 3);
            let mut parts = body.split('|').map(|part| part.trim().to_owned());
            let question = parts.next().unwrap_or_default();
            let options: Vec<String> = parts.filter(|part| !part.is_empty()).collect();
            vec![Event::Poll {
                peer: club(),
                author: author.into(),
                name: name.into(),
                sent,
                question,
                options,
                multiple: command == "pollmulti",
                outgoing: false,
            }]
        }
        "vote" | "unvote" => {
            let Some((by, _)) = who(next()) else {
                return Vec::new();
            };
            let author = match next() {
                "me" => ME,
                other => who(other).map_or(ME, |(id, _)| id),
            };
            let target = next().parse().unwrap_or(0);
            let indexes = if command == "unvote" { Vec::new() } else { next().split(',').filter_map(|part| part.parse().ok()).collect() };
            vec![Event::PollVote { peer: club(), by: by.into(), author: author.into(), target, indexes, serial: 0 }]
        }
        "endpoll" => {
            let author = match next() {
                "me" => ME.to_owned(),
                other => who(other).map(|(id, _)| id.to_owned()).unwrap_or_default(),
            };
            if author.is_empty() {
                return Vec::new();
            }
            let target = next().parse().unwrap_or(0);
            vec![Event::PollEnded { peer: club(), author, target }]
        }
        "read" => {
            let Some((author, _)) = who(next()) else {
                return Vec::new();
            };
            vec![Event::ReadElsewhere(vec![(author.into(), next().parse().unwrap_or(0))])]
        }
        "seen" | "delivered" | "viewed" => {
            let Some((by, _)) = who(next()) else {
                return Vec::new();
            };
            let sent: Vec<u64> = next().split(',').filter_map(|part| part.parse().ok()).collect();
            let at = sent.iter().copied().max().unwrap_or(0).saturating_add(60_000);
            vec![Event::Receipt { by: by.into(), sent, read: command != "delivered", viewed: command == "viewed", at }]
        }
        "receipts" => {
            let read_receipts = next() != "off";
            vec![Event::Settings { read_receipts, typing: true }]
        }
        "typing" | "typing-stop" => {
            let Some((by, name)) = who(next()) else {
                return Vec::new();
            };
            vec![Event::Typing { peer: direct(by), by: by.into(), name: name.into(), started: command == "typing" }]
        }
        "call" | "call-video" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            vec![Event::Call { peer: direct(author), by: author.into(), name: name.into(), video: command == "call-video", id: sent, sent }]
        }
        "hangup" | "hangup-accepted" | "busy" => {
            let id = next().parse().unwrap_or(T0);
            vec![Event::CallEnded { id, answered: command == "hangup-accepted" }]
        }
        "group-title" => {
            let title = rest(line, 1);
            if title.is_empty() {
                return Vec::new();
            }
            vec![Event::GroupChanged {
                peer: club(),
                by: String::new(),
                changes: vec![format!("The group name is now {title}")],
                title: Some(title),
                description: None,
                left: false,
                rejoined: false,
            }]
        }
        "group-leave" => vec![Event::GroupChanged {
            peer: club(),
            by: String::new(),
            changes: vec!["You are no longer in this group".to_owned()],
            title: None,
            description: None,
            left: true,
            rejoined: false,
        }],
        "group-rejoin" => vec![Event::GroupChanged {
            peer: club(),
            by: String::new(),
            changes: vec!["You joined the group".to_owned()],
            title: None,
            description: None,
            left: false,
            rejoined: true,
        }],
        "chat-state" => {
            let target = next();
            let peer = if target == "club" {
                club()
            } else if let Some((id, _)) = who(target) {
                direct(id)
            } else {
                return Vec::new();
            };
            let mut state = ChatState {
                peer,
                muted_until: 0,
                dont_notify_for_mentions_if_muted: false,
                archived: false,
                marked_unread: false,
                blocked: false,
                whitelisted: true,
            };
            for token in words {
                if let Some((k, v)) = token.split_once('=') {
                    match k {
                        "muted" => state.muted_until = v.parse().unwrap_or(0),
                        "dont_notify_mentions" => state.dont_notify_for_mentions_if_muted = v == "true" || v == "1",
                        "archived" => state.archived = v == "true" || v == "1",
                        "unread" => state.marked_unread = v == "true" || v == "1",
                        "blocked" => state.blocked = v == "true" || v == "1",
                        "whitelisted" => state.whitelisted = v == "true" || v == "1",
                        _ => {}
                    }
                }
            }
            vec![Event::ChatStates { chats: vec![state], pinned: None, keep_muted_archived: None }]
        }
        "pin" => {
            let mut pinned = Vec::new();
            for target in words {
                if target == "club" {
                    pinned.push(club());
                } else if let Some((id, _)) = who(target) {
                    pinned.push(direct(id));
                }
            }
            vec![Event::ChatStates { chats: Vec::new(), pinned: Some(pinned), keep_muted_archived: None }]
        }
        "keep-muted-archived" => {
            let on = next() == "on";
            vec![Event::ChatStates { chats: Vec::new(), pinned: None, keep_muted_archived: Some(on) }]
        }
        "folders" => {
            // Format: folders id:Name:opts... ; id2:Name2:opts...
            // Where opts are comma-separated key=val: include=grace+club,exclude=alan,all_dm=1,all_group=1,only_unread=1,show_muted=0
            let spec = line.strip_prefix("folders ").unwrap_or("");
            let parse_peer = |tok: &str| -> Option<Peer> {
                if tok == "club" {
                    Some(club())
                } else {
                    who(tok).map(|(id, _)| direct(id))
                }
            };
            let mut list = Vec::new();
            for (pos, entry) in spec.split(';').map(str::trim).filter(|s| !s.is_empty()).enumerate() {
                let mut parts = entry.splitn(3, ':');
                let id = parts.next().unwrap_or("").trim().to_owned();
                let name = parts.next().unwrap_or("").trim().to_owned();
                let opts = parts.next().unwrap_or("");
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let mut folder = ChatFolder {
                    id,
                    name,
                    position: pos as u32,
                    show_only_unread: false,
                    show_muted: true,
                    include_all_individual: false,
                    include_all_groups: false,
                    included: Vec::new(),
                    excluded: Vec::new(),
                };
                for kv in opts.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if let Some((k, v)) = kv.split_once('=') {
                        match k {
                            "include" => folder.included = v.split('+').filter_map(parse_peer).collect(),
                            "exclude" => folder.excluded = v.split('+').filter_map(parse_peer).collect(),
                            "all_dm" => folder.include_all_individual = v == "1" || v == "true",
                            "all_group" => folder.include_all_groups = v == "1" || v == "true",
                            "only_unread" => folder.show_only_unread = v == "1" || v == "true",
                            "show_muted" => folder.show_muted = v == "1" || v == "true",
                            _ => {}
                        }
                    }
                }
                list.push(folder);
            }
            vec![Event::Folders(list)]
        }
        "stranger" => {
            let sent = next().parse().unwrap_or(T0);
            let text = Text {
                author: STRANGER.into(),
                name: "Sam Stranger".into(),
                sent,
                text: rest(line, 2),
                ..Text::default()
            };
            vec![Event::Message { peer: direct(STRANGER), text, outgoing: false }]
        }
        "blocked" => {
            let mut people = Vec::new();
            let mut groups = Vec::new();
            for token in words {
                if token == "club" {
                    groups.push(CLUB.to_owned());
                } else if token == "stranger" {
                    people.push(STRANGER.to_owned());
                } else if let Some((id, _)) = who(token) {
                    people.push(id.to_owned());
                } else if super::wire::is_aci(token) {
                    people.push(token.to_owned());
                }
            }
            vec![Event::Blocked { people, groups }]
        }
        "request-response" => {
            let target = next();
            let peer = if target == "club" {
                club()
            } else if target == "stranger" {
                direct(STRANGER)
            } else if let Some((id, _)) = who(target) {
                direct(id)
            } else if super::wire::is_aci(target) {
                direct(target)
            } else {
                return Vec::new();
            };
            let action = match next() {
                "accept" => MessageRequestAction::Accept,
                "delete" => MessageRequestAction::Delete,
                "block" => MessageRequestAction::Block,
                "block-delete" => MessageRequestAction::BlockAndDelete,
                "spam" => MessageRequestAction::Spam,
                "block-spam" => MessageRequestAction::BlockAndSpam,
                _ => return Vec::new(),
            };
            vec![Event::MessageRequestResponse { peer, action }]
        }
        "drop" => vec![Event::Closed("the fake dropped the connection".into())],
        "unlinked" => vec![Event::Ended("This computer was removed from your Signal account.".into())],
        _ => Vec::new(),
    }
}

fn describe(to: &Peer) -> String {
    to.conversation()
}

/// Style ranges on a send, empty when there are none so an unstyled send reads as before.
fn noted(ranges: &[super::style::Range]) -> String {
    let parts: Vec<String> = ranges
        .iter()
        .map(|range| match range {
            super::style::Range::Style { start, length, style } => format!("{style:?}:{start}+{length}"),
            super::style::Range::Mention { start, aci, .. } => format!("Mention({aci}):{start}+1"),
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ranges={}", parts.join(","))
    }
}

impl Wire for Fake {
    fn events(&self) -> Option<Receiver<Event>> {
        self.heard.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }

    fn me(&self) -> (String, String) {
        let name = self.card.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).name.clone();
        (ME.to_owned(), name)
    }

    fn send(&self, to: &Peer, body: Body) -> Result<u64, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let exp = |timer: super::wire::Timer| {
            if timer.seconds > 0 { format!(" expire={}", timer.seconds) } else { String::new() }
        };
        let line = match &body {
            Body::Text { text, quote: Some(quote), ranges, timer } => {
                format!("send {} text {text}{}{} (answering {}-{})", describe(to), noted(ranges), exp(*timer), quote.sent, quote.author)
            }
            Body::Text { text, quote: None, ranges, timer } => format!("send {} text {text}{}{}", describe(to), noted(ranges), exp(*timer)),
            Body::Files { text, quote, files, timer } => {
                let names: Vec<String> = files.iter().map(|file| file.name.clone()).collect();
                format!("{}{}", super::files::call_line(&describe(to), &names, text, quote.as_ref()), exp(*timer))
            }
            Body::React { author, target, emoji, remove } => format!("send {} react {target}-{author} {emoji} remove={remove}", describe(to)),
            Body::Edit { target, text, ranges, timer } => format!("send {} edit {target} {text}{}{}", describe(to), noted(ranges), exp(*timer)),
            Body::Delete { target } => format!("send {} delete {target}", describe(to)),
            Body::DeleteForMe { target, .. } => format!("send {} delete-for-me {target}", describe(to)),
            Body::Poll { question, options, multiple, timer } => format!("send {} poll {question} | {} multi={multiple}{}", describe(to), options.join(" | "), exp(*timer)),
            Body::PollVote { author, target, indexes, serial } => {
                let picked = if indexes.is_empty() { "-".to_owned() } else { indexes.iter().map(|index| index.to_string()).collect::<Vec<_>>().join(",") };
                format!("send {} vote {target}-{author} {picked} serial={serial}", describe(to))
            }
            Body::PollEnd { target } => format!("send {} endpoll {target}", describe(to)),
        };
        record(&line);
        if let Some(words) = self.refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        if matches!(to, Peer::Group(_)) {
            if let Some(person) = self.fail_one.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
                self.missed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(person);
            }
        }
        Ok(self.clock.fetch_add(1_000, Ordering::Relaxed))
    }

    fn missed(&self) -> Vec<String> {
        std::mem::take(&mut *self.missed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
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

    fn card(&self) -> Result<OwnCard, Fault> {
        record("profile read");
        Ok(self.card.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone())
    }

    fn set_card(&self, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let mut card = self.card.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(name) = name {
            card.name = name;
        }
        if let Some(about) = about {
            card.about = about;
        }
        if let Some(emoji) = emoji {
            card.emoji = emoji;
        }
        record(&format!("profile name={} about={} emoji={} sync=local-profile", card.name, card.about, card.emoji));
        Ok(())
    }

    /// Grace has a profile picture (one pixel); nobody else has one.
    fn picture(&self, of: &Peer) -> Result<Option<Vec<u8>>, Fault> {
        record(&format!("picture {}", describe(of)));
        const PNG: &str = "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000c4944415408d763f8cfc0000000020001e221bc330000000049454e44ae426082";
        let png = (0..PNG.len()).step_by(2).filter_map(|at| u8::from_str_radix(&PNG[at..at + 2], 16).ok()).collect();
        Ok((*of == Peer::Direct(GRACE.to_owned())).then_some(png))
    }

    fn lookup(&self, username: &str) -> Result<Option<String>, Fault> {
        record(&format!("lookup {username}"));
        let found = self.lookups.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&username.to_ascii_lowercase()).cloned();
        Ok(found)
    }

    fn set_mute(&self, to: &Peer, until_ms: u64) -> Result<(), Fault> {
        record(&format!("mute {} {until_ms}", describe(to)));
        Ok(())
    }

    fn set_archived(&self, to: &Peer, archived: bool) -> Result<(), Fault> {
        record(&format!("archive {} {archived}", describe(to)));
        Ok(())
    }

    fn set_pinned(&self, to: &Peer, pinned: bool) -> Result<(), Fault> {
        record(&format!("pin {} {pinned}", describe(to)));
        Ok(())
    }

    fn set_marked_unread(&self, to: &Peer, unread: bool) -> Result<(), Fault> {
        record(&format!("mark-unread {} {unread}", describe(to)));
        Ok(())
    }

    fn set_blocked(&self, peer: &Peer, blocked: bool) -> Result<(), Fault> {
        record(&format!("{} {}", if blocked { "block" } else { "unblock" }, describe(peer)));
        Ok(())
    }

    fn respond_message_request(&self, peer: &Peer, action: MessageRequestAction) -> Result<(), Fault> {
        let label = match action {
            MessageRequestAction::Accept => "accept",
            MessageRequestAction::Delete => "delete",
            MessageRequestAction::Block => "block",
            MessageRequestAction::BlockAndDelete => "block-delete",
            MessageRequestAction::Spam => "spam",
            MessageRequestAction::BlockAndSpam => "block-spam",
        };
        record(&format!("request-response {} {label}", describe(peer)));
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
