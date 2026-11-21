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

use super::wire::{call_link_room_id, format_call_link, format_group_invite_link, gift_badge_label, parse_call_link, parse_group_invite_link, payment_label, view_once_label, Body, CallKind, CallLink, CallLinkJoin, CallLogKind, CallOutcome, Contact, CONTINUES, Event, Fault, GifEntry, GroupAccess, GroupEdit, GroupInfo, GroupMember, GroupMembers, InviteLinkMode, Linked, Linker, OwnCard, Peer, Quote, Text, Wire};
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

#[derive(Clone)]
struct FakeGroup {
    info: GroupInfo,
    members: Vec<GroupMember>,
    pending: Vec<GroupMember>,
    requesting: Vec<GroupMember>,
}

impl FakeGroup {
    fn initial() -> Self {
        let members = vec![
            GroupMember { id: ME.into(), name: "Ada Quill".into(), admin: true },
            GroupMember { id: GRACE.into(), name: "Grace Ombra".into(), admin: false },
            GroupMember { id: ALAN.into(), name: "Alan Ferro".into(), admin: false },
        ];
        let mut group = Self {
            info: GroupInfo {
                id: CLUB.into(),
                title: "Bridge club".into(),
                description: "Tuesdays, 7 pm".into(),
                members: 3,
                member: true,
                member_ids: vec![ME.into(), GRACE.into(), ALAN.into()],
                admins: vec![ME.into()],
                member_access: GroupAccess::Member,
                attributes_access: GroupAccess::Member,
                invite_link_access: GroupAccess::Unsatisfiable,
                announcements_only: false,
                invite_link: String::new(),
                timer_secs: 0,
            },
            members,
            pending: Vec::new(),
            requesting: Vec::new(),
        };
        group.sync_info();
        group
    }

    fn sync_info(&mut self) {
        self.info.members = self.members.len() as u32;
        self.info.member = self.members.iter().any(|m| m.id == ME);
        self.info.member_ids = self.members.iter().map(|m| m.id.clone()).collect();
        self.info.admins = self.members.iter().filter(|m| m.admin).map(|m| m.id.clone()).collect();
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
    /// Mutable state of the fake `CLUB` group.
    club: Arc<Mutex<FakeGroup>>,
    /// Fake call links created or synced in tests.
    call_links: Arc<Mutex<HashMap<[u8; 32], CallLink>>>,
}

impl Fake {
    pub fn open(_saved: &Saved) -> Fake {
        let (events, heard) = channel();
        // What the phone says once a device connects: its contacts, its groups, its settings.
        let grace = Contact { id: GRACE.into(), name: "Grace Ombra".into(), phone: "+15550100002".into() };
        let alan = Contact { id: ALAN.into(), name: "Alan Ferro".into(), phone: String::new() };
        let _ = events.send(Event::Contacts(vec![grace, alan]));
        let club_state = FakeGroup::initial();
        let _ = events.send(Event::Groups(vec![club_state.info.clone()]));
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
            club: Arc::new(Mutex::new(club_state)),
            call_links: Arc::default(),
        };
        fake.listen();
        fake
    }

    /// Reads the test's lines for as long as this wire lives; closing the wire ends it.
    fn listen(&self) {
        let (events, closed, refuse, fail_one, control, lookups, club) = (
            self.events.clone(),
            self.closed.clone(),
            self.refuse.clone(),
            self.fail_one.clone(),
            self.control.clone(),
            self.lookups.clone(),
            self.club.clone(),
        );
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
                if let Some(rest) = line.strip_prefix("group-role ") {
                    let mut parts = rest.split_whitespace();
                    let target = match parts.next().unwrap_or("") {
                        "me" => Some(ME),
                        other => who(other).map(|(id, _)| id),
                    };
                    let admin = parts.next().unwrap_or("") == "admin";
                    if let Some(target) = target {
                        let mut g = club.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(m) = g.members.iter_mut().find(|m| m.id == target) {
                            m.admin = admin;
                        }
                        g.sync_info();
                        let _ = events.send(Event::Groups(vec![g.info.clone()]));
                    }
                    return true;
                }
                if let Some(rest) = line.strip_prefix("group-pending ") {
                    let mut parts = rest.splitn(2, ' ');
                    let aci = parts.next().unwrap_or("").trim();
                    let name = parts.next().unwrap_or("").trim();
                    if super::wire::is_aci(aci) {
                        let mut g = club.lock().unwrap_or_else(|p| p.into_inner());
                        if !g.pending.iter().any(|m| m.id == aci) {
                            g.pending.push(GroupMember { id: aci.to_owned(), name: name.to_owned(), admin: false });
                        }
                    }
                    return true;
                }
                if let Some(rest) = line.strip_prefix("group-requesting ") {
                    let mut parts = rest.splitn(2, ' ');
                    let aci = parts.next().unwrap_or("").trim();
                    let name = parts.next().unwrap_or("").trim();
                    if super::wire::is_aci(aci) {
                        let mut g = club.lock().unwrap_or_else(|p| p.into_inner());
                        if !g.requesting.iter().any(|m| m.id == aci) {
                            g.requesting.push(GroupMember { id: aci.to_owned(), name: name.to_owned(), admin: false });
                        }
                    }
                    return true;
                }
                if let Some(rest) = line.strip_prefix("group-access ") {
                    let mut g = club.lock().unwrap_or_else(|p| p.into_inner());
                    for token in rest.split_whitespace() {
                        if let Some((k, v)) = token.split_once('=') {
                            match (k, v) {
                                ("members", "admin") => g.info.member_access = GroupAccess::Administrator,
                                ("members", "member") => g.info.member_access = GroupAccess::Member,
                                ("attributes", "admin") => g.info.attributes_access = GroupAccess::Administrator,
                                ("attributes", "member") => g.info.attributes_access = GroupAccess::Member,
                                ("link", "off") => {
                                    g.info.invite_link_access = GroupAccess::Unsatisfiable;
                                    g.info.invite_link.clear();
                                }
                                ("link", "on") => {
                                    g.info.invite_link_access = GroupAccess::Any;
                                    g.info.invite_link = "https://signal.group/#CjQKIFoXWhdaF1oXWhdaF1oXWhdaF1oXWhdaF1oXWhdaF1oXEhBwbGFjZWhvbGRlcl9wYXNz".into();
                                }
                                ("link", "approval") => {
                                    g.info.invite_link_access = GroupAccess::Administrator;
                                    g.info.invite_link = "https://signal.group/#CjQKIFoXWhdaF1oXWhdaF1oXWhdaF1oXWhdaF1oXWhdaF1oXEhBwbGFjZWhvbGRlcl9wYXNz".into();
                                }
                                ("announcements", "on") => g.info.announcements_only = true,
                                ("announcements", "off") => g.info.announcements_only = false,
                                _ => {}
                            }
                        }
                    }
                    let _ = events.send(Event::Groups(vec![g.info.clone()]));
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
        "call-event" => {
            let target = next();
            let peer = if target == "club" {
                club()
            } else if let Some((id, _)) = who(target) {
                direct(id)
            } else {
                return Vec::new();
            };
            let call_id = next().parse().unwrap_or(T0);
            let kind = match next() {
                "video" => CallKind::Video,
                "group" => CallKind::Group,
                "adhoc" => CallKind::AdHoc,
                _ => CallKind::Voice,
            };
            let incoming = next() != "outgoing";
            let outcome = match next() {
                "missed" => CallOutcome::NotAccepted,
                "deleted" => CallOutcome::Deleted,
                "observed" => CallOutcome::Observed,
                _ => CallOutcome::Accepted,
            };
            let timestamp = next().parse().unwrap_or(call_id);
            vec![Event::CallEvent { peer, call_id, timestamp, kind, incoming, outcome }]
        }
        "call-log" => {
            let kind = match next() {
                "clear" => CallLogKind::Clear,
                "read" => CallLogKind::MarkedAsRead,
                "clear-chat" => CallLogKind::ClearInConversation,
                "read-chat" => CallLogKind::MarkedAsReadInConversation,
                _ => return Vec::new(),
            };
            let peer = match next() {
                "" => None,
                "club" => Some(club()),
                other => who(other).map(|(id, _)| direct(id)),
            };
            vec![Event::CallLogEvent { kind, peer, call_id: None, timestamp: T0 }]
        }
        "call-link-sync" => {
            let byte = next().parse::<u8>().unwrap_or(0x42);
            vec![Event::CallLinkUpdated { root_key: [byte; 32], admin_passkey: Some(vec![0xaa; 32]) }]
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

    fn group_members(&self, group_id: &str) -> Result<GroupMembers, Fault> {
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        Ok(GroupMembers {
            members: g.members.clone(),
            pending: g.pending.clone(),
            requesting: g.requesting.clone(),
        })
    }

    fn group_add_members(&self, group_id: &str, people: &[String]) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        record(&format!("group-add {} {}", Peer::Group(group_id.to_owned()).conversation(), people.join(",")));
        if let Some(words) = self.refuse.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        for aci in people {
            if !g.members.iter().any(|m| m.id == *aci) {
                g.members.push(GroupMember { id: aci.clone(), name: String::new(), admin: false });
            }
            g.pending.retain(|m| m.id != *aci);
            g.requesting.retain(|m| m.id != *aci);
        }
        g.sync_info();
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn group_remove_member(&self, group_id: &str, person: &str) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        record(&format!("group-remove {} {person}", Peer::Group(group_id.to_owned()).conversation()));
        if let Some(words) = self.refuse.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        g.members.retain(|m| m.id != person);
        g.pending.retain(|m| m.id != person);
        g.requesting.retain(|m| m.id != person);
        g.sync_info();
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn group_set_admin(&self, group_id: &str, person: &str, admin: bool) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        record(&format!("group-role {} {person} {}", Peer::Group(group_id.to_owned()).conversation(), if admin { "admin" } else { "member" }));
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = g.members.iter_mut().find(|m| m.id == person) {
            m.admin = admin;
        }
        g.sync_info();
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn group_update(&self, group_id: &str, edit: GroupEdit) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let conv = Peer::Group(group_id.to_owned()).conversation();
        let mut parts = Vec::new();
        if let Some(t) = &edit.title {
            parts.push(format!("title={t}"));
        }
        if let Some(d) = &edit.description {
            parts.push(format!("description={}", d.chars().take(40).collect::<String>()));
        }
        if let Some(ma) = edit.member_access {
            parts.push(format!("members={ma:?}"));
        }
        if let Some(aa) = edit.attributes_access {
            parts.push(format!("attributes={aa:?}"));
        }
        if let Some(ao) = edit.announcements_only {
            parts.push(format!("announcements={ao}"));
        }
        record(&format!("group-update {conv} {}", parts.join(" ")));
        if let Some(words) = self.refuse.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(title) = edit.title {
            g.info.title = title;
        }
        if let Some(desc) = edit.description {
            g.info.description = desc;
        }
        if let Some(ma) = edit.member_access {
            g.info.member_access = ma;
        }
        if let Some(aa) = edit.attributes_access {
            g.info.attributes_access = aa;
        }
        if let Some(ao) = edit.announcements_only {
            g.info.announcements_only = ao;
        }
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn group_set_avatar(&self, group_id: &str, bytes: Vec<u8>) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let conv = Peer::Group(group_id.to_owned()).conversation();
        record(&format!("group-avatar {conv} {}", if bytes.is_empty() { "clear".to_owned() } else { format!("{}bytes", bytes.len()) }));
        if let Some(words) = self.refuse.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        Ok(())
    }

    fn group_set_timer(&self, group_id: &str, seconds: u32) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let conv = Peer::Group(group_id.to_owned()).conversation();
        record(&format!("group-timer {conv} {seconds}"));
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        g.info.timer_secs = seconds;
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn group_invite_link(&self, group_id: &str, mode: InviteLinkMode) -> Result<String, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let conv = Peer::Group(group_id.to_owned()).conversation();
        record(&format!("group-link {conv} {mode:?}"));
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        let key = [0x5au8; 32];
        match mode {
            InviteLinkMode::Off => {
                g.info.invite_link_access = GroupAccess::Unsatisfiable;
                g.info.invite_link.clear();
            }
            InviteLinkMode::Enabled => {
                g.info.invite_link_access = GroupAccess::Any;
                g.info.invite_link = format_group_invite_link(&key, b"club-pass-v1");
            }
            InviteLinkMode::ApprovalRequired => {
                g.info.invite_link_access = GroupAccess::Administrator;
                g.info.invite_link = format_group_invite_link(&key, b"club-pass-v1");
            }
            InviteLinkMode::Reset => {
                if g.info.invite_link_access == GroupAccess::Unsatisfiable {
                    g.info.invite_link_access = GroupAccess::Any;
                }
                g.info.invite_link = format_group_invite_link(&key, b"club-pass-v2");
            }
        }
        let link = g.info.invite_link.clone();
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(link)
    }

    fn group_join_by_link(&self, url: &str) -> Result<GroupInfo, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let Some(_parsed) = parse_group_invite_link(url) else {
            return Err(Fault::Refused("That is not a valid Signal group link.".into()));
        };
        record("group-join-link");
        let joined = GroupInfo {
            id: "6b286b286b286b286b286b286b286b286b286b286b286b286b286b286b286b28".into(),
            title: "Chess club".into(),
            description: "Joined via invite link".into(),
            members: 2,
            member: true,
            member_ids: vec![ME.into(), GRACE.into()],
            admins: vec![GRACE.into()],
            member_access: GroupAccess::Member,
            attributes_access: GroupAccess::Administrator,
            invite_link_access: GroupAccess::Any,
            announcements_only: false,
            invite_link: url.to_owned(),
            timer_secs: 0,
        };
        let _ = self.events.send(Event::Groups(vec![joined.clone()]));
        Ok(joined)
    }

    fn group_review_request(&self, group_id: &str, person: &str, approve: bool) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let conv = Peer::Group(group_id.to_owned()).conversation();
        record(&format!("group-request {conv} {person} {}", if approve { "approve" } else { "deny" }));
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        let req = g.requesting.iter().position(|m| m.id == person).map(|idx| g.requesting.remove(idx));
        if approve {
            let member = req.unwrap_or(GroupMember { id: person.to_owned(), name: String::new(), admin: false });
            if !g.members.iter().any(|m| m.id == person) {
                g.members.push(member);
            }
        }
        g.sync_info();
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn group_pending_invite(&self, group_id: &str, accept: bool) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if group_id != CLUB {
            return Err(Fault::Refused("This group isn't one Signal still knows for you.".into()));
        }
        let conv = Peer::Group(group_id.to_owned()).conversation();
        record(&format!("group-invite {conv} {}", if accept { "accept" } else { "decline" }));
        let mut g = self.club.lock().unwrap_or_else(|p| p.into_inner());
        g.pending.retain(|m| m.id != ME);
        if accept {
            if !g.members.iter().any(|m| m.id == ME) {
                g.members.push(GroupMember { id: ME.into(), name: "Ada Quill".into(), admin: false });
            }
        } else {
            g.members.retain(|m| m.id != ME);
        }
        g.sync_info();
        let _ = self.events.send(Event::Groups(vec![g.info.clone()]));
        Ok(())
    }

    fn search_gifs(&self, query: &str, offset: u32) -> Result<(Vec<GifEntry>, Option<u32>), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        record(&format!("gif-search offset={offset} query={query}"));
        let q = query.trim();
        let entry = GifEntry {
            id: if q.is_empty() { "trending-1".into() } else { format!("gif-{q}") },
            title: if q.is_empty() { "Trending wave".into() } else { format!("{q} reaction") },
            preview_url: format!("https://media.giphy.com/media/{}/200w_s.gif", if q.is_empty() { "trending-1" } else { q }),
            mp4_url: format!("https://media.giphy.com/media/{}/200w.mp4", if q.is_empty() { "trending-1" } else { q }),
            width: 200,
            height: 150,
        };
        Ok((vec![entry], None))
    }

    fn fetch_gif(&self, url: &str) -> Result<Vec<u8>, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        record(&format!("gif-fetch {url}"));
        // Minimal valid ISO-BMFF/MP4 header bytes.
        Ok(b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom".to_vec())
    }

    fn create_call_link(&self, name: &str, approve_all: bool) -> Result<CallLink, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let seq = (self.clock.fetch_add(1, Ordering::Relaxed) & 0xff) as u8;
        let root_key = [seq.max(1); 32];
        let link = CallLink {
            root_key,
            url: format_call_link(&root_key),
            name: name.to_owned(),
            approve_all,
            admin_passkey: Some(vec![0xab; 32]),
        };
        record(&format!("call-link-create name={name} approve={approve_all}"));
        self.call_links.lock().unwrap_or_else(|p| p.into_inner()).insert(root_key, link.clone());
        Ok(link)
    }

    fn update_call_link(&self, root_key: &[u8; 32], name: Option<String>, approve_all: Option<bool>) -> Result<CallLink, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let mut map = self.call_links.lock().unwrap_or_else(|p| p.into_inner());
        let entry = map.entry(*root_key).or_insert_with(|| CallLink {
            root_key: *root_key,
            url: format_call_link(root_key),
            name: String::new(),
            approve_all: false,
            admin_passkey: Some(vec![0xab; 32]),
        });
        if let Some(n) = name {
            entry.name = n;
        }
        if let Some(a) = approve_all {
            entry.approve_all = a;
        }
        record(&format!("call-link-update name={} approve={}", entry.name, entry.approve_all));
        Ok(entry.clone())
    }

    fn delete_call_link(&self, root_key: &[u8; 32]) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        self.call_links.lock().unwrap_or_else(|p| p.into_inner()).remove(root_key);
        record("call-link-delete");
        Ok(())
    }

    fn join_call_link(&self, url: &str) -> Result<CallLinkJoin, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let Some(root_key) = parse_call_link(url) else {
            return Err(Fault::Refused("That is not a valid Signal call link.".into()));
        };
        let room_id = call_link_room_id(&root_key);
        record(&format!("call-link-join {room_id}"));
        let stored = self.call_links.lock().unwrap_or_else(|p| p.into_inner()).get(&root_key).cloned();
        Ok(CallLinkJoin {
            room_id,
            name: stored.as_ref().map(|l| l.name.clone()).unwrap_or_default(),
            waiting_for_approval: stored.as_ref().is_some_and(|l| l.approve_all),
        })
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
