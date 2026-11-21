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

use super::wire::{
    call_link_room_id, format_call_link, format_group_invite_link, gift_badge_label, parse_call_link,
    parse_group_invite_link, payment_label, view_once_label, Body, CallKind, CallLink, CallLinkJoin, CallLogKind,
    CallOutcome, ChatFolder, ChatState, Contact, CONTINUES, Event, Fault, GifEntry, GroupAccess, GroupEdit, GroupInfo,
    GroupMember, GroupMembers, IdentityState, InviteLinkMode, LinkPreview, Linked, Linker, MessageRequestAction, OwnCard,
    Peer, PersonProfile, Pointer, Quote, QuotedAttachment, SafetyNumber, StorageSnapshot, Text, Wire, FLAG_GIF,
};
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

#[derive(Clone)]
struct FakeIdentity {
    key: [u8; 33],
    state: IdentityState,
    changed: bool,
}

fn fake_key(seed: u8) -> [u8; 33] {
    let mut key = [seed; 33];
    key[0] = 0x05;
    key
}

fn initial_identities() -> HashMap<String, FakeIdentity> {
    let mut map = HashMap::new();
    map.insert(GRACE.to_owned(), FakeIdentity { key: fake_key(0x22), state: IdentityState::Default, changed: false });
    map.insert(ALAN.to_owned(), FakeIdentity { key: fake_key(0x33), state: IdentityState::Default, changed: false });
    map.insert(STRANGER.to_owned(), FakeIdentity { key: fake_key(0x44), state: IdentityState::Default, changed: false });
    map
}

fn initial_profiles() -> HashMap<String, PersonProfile> {
    let mut map = HashMap::new();
    map.insert(
        GRACE.to_owned(),
        PersonProfile {
            name: "Grace Ombra".into(),
            about: "Likes bridge".into(),
            emoji: "🃏".into(),
            badges: vec!["Sustainer".into()],
            key_known: true,
        },
    );
    map.insert(
        ALAN.to_owned(),
        PersonProfile {
            name: "Alan Ferro".into(),
            about: "Leads trumps".into(),
            emoji: "♠️".into(),
            badges: Vec::new(),
            key_known: true,
        },
    );
    map.insert(
        STRANGER.to_owned(),
        PersonProfile {
            name: String::new(),
            about: String::new(),
            emoji: String::new(),
            badges: Vec::new(),
            key_known: false,
        },
    );
    map
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
    /// Per-contact identity keys and verification states for safety numbers.
    identities: Arc<Mutex<HashMap<String, FakeIdentity>>>,
    /// Scripted decrypted profiles for other Signal accounts.
    profiles: Arc<Mutex<HashMap<String, PersonProfile>>>,
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
        let _ = events.send(Event::Settings { read_receipts: true, typing: true, link_previews: Some(true) });
        let manifest_path = protocol::support_dir().join("signal.storage.json");
        if let Ok(raw) = std::fs::read_to_string(&manifest_path) {
            if let Ok(snapshot) = serde_json::from_str::<StorageSnapshot>(&raw) {
                let _ = events.send(Event::StorageSync(snapshot));
            }
        }
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
            identities: Arc::new(Mutex::new(initial_identities())),
            profiles: Arc::new(Mutex::new(initial_profiles())),
        };
        fake.listen();
        fake
    }

    /// Reads the test's lines for as long as this wire lives; closing the wire ends it.
    fn listen(&self) {
        let (events, closed, refuse, fail_one, control, lookups, club, identities, profiles) = (
            self.events.clone(),
            self.closed.clone(),
            self.refuse.clone(),
            self.fail_one.clone(),
            self.control.clone(),
            self.lookups.clone(),
            self.club.clone(),
            self.identities.clone(),
            self.profiles.clone(),
        );
        let _ = std::thread::Builder::new().name("signal-fake".into()).spawn(move || {
            script::follow(&control, |line| {
                if closed.load(Ordering::Relaxed) {
                    return false;
                }
                let Some(line) = line.map(str::trim) else {
                    return true;
                };
                if let Some(rest) = line.strip_prefix("profile ") {
                    let mut parts = rest.splitn(2, ' ');
                    let target = parts.next().unwrap_or("").trim();
                    let body = parts.next().unwrap_or("");
                    let aci = if let Some((id, _)) = who(target) {
                        Some(id.to_owned())
                    } else if target == "stranger" {
                        Some(STRANGER.to_owned())
                    } else if super::wire::is_aci(target) {
                        Some(target.to_owned())
                    } else {
                        None
                    };
                    if let Some(aci) = aci {
                        let fields: Vec<&str> = body.split('|').map(str::trim).collect();
                        let mut map = profiles.lock().unwrap_or_else(|p| p.into_inner());
                        let entry = map.entry(aci).or_default();
                        entry.key_known = true;
                        entry.about = fields.first().copied().unwrap_or("").to_owned();
                        entry.emoji = fields.get(1).copied().unwrap_or("").to_owned();
                        entry.badges = fields
                            .get(2)
                            .copied()
                            .unwrap_or("")
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect();
                    }
                    return true;
                }
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
                for event in steer(line, &identities) {
                    let _ = events.send(event);
                }
                true
            });
        });
    }
}

/// One line from the test: what Signal then says unasked.
fn steer(line: &str, identities: &Mutex<HashMap<String, FakeIdentity>>) -> Vec<Event> {
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
                "photo" => {
                    text.attachments.push(Pointer {
                        cdn_key: Some("fake-photo".to_owned()),
                        content_type: "image/jpeg".to_owned(),
                        file_name: "photo.jpg".to_owned(),
                        width: 640,
                        height: 480,
                        size: 67,
                        blurhash: Some("00TI:j".to_owned()),
                        ..Pointer::default()
                    });
                }
                "payment" => {
                    let note = rest(line, 3);
                    text.unsupported = Some(payment_label((!note.is_empty()).then_some(note.as_str())));
                }
                "giftbadge" => text.unsupported = Some(gift_badge_label().to_owned()),
                "viewonce" => text.unsupported = Some(view_once_label(Some("image/jpeg")).to_owned()),
                "viewonce-video" => text.unsupported = Some(view_once_label(Some("video/mp4")).to_owned()),
                "quote" => {
                    let quoted = next().parse().unwrap_or(0);
                    text.quote = Some(Quote {
                        sent: quoted,
                        author: ME.into(),
                        text: String::new(),
                        spans: Vec::new(),
                        attachments: Vec::new(),
                    });
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
            vec![Event::Settings { read_receipts, typing: true, link_previews: None }]
        }
        "link-previews" => {
            let enabled = next() != "off";
            vec![Event::Settings { read_receipts: true, typing: true, link_previews: Some(enabled) }]
        }
        "preview" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let payload = rest(line, 3);
            let parts: Vec<&str> = payload.split('|').map(str::trim).collect();
            let url = parts.first().copied().unwrap_or("").to_owned();
            let title = parts.get(1).copied().unwrap_or("").to_owned();
            let description = parts.get(2).copied().unwrap_or("").to_owned();
            let body = parts.get(3).copied().unwrap_or("").to_owned();
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                text: body,
                previews: vec![LinkPreview {
                    url,
                    title,
                    description,
                    date: sent,
                    image: None,
                    image_width: 0,
                    image_height: 0,
                }],
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
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
        "storage-manifest" => {
            let json = line.strip_prefix("storage-manifest ").unwrap_or("").trim();
            match serde_json::from_str::<StorageSnapshot>(json) {
                Ok(snapshot) => vec![Event::StorageSync(snapshot)],
                Err(_) => Vec::new(),
            }
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
        "contact" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let spec = rest(line, 3);
            let mut parts = spec.split('|').map(|p| p.trim().to_owned());
            let card_name = parts.next().unwrap_or_default();
            let phone = parts.next().unwrap_or_default();
            let email = parts.next().unwrap_or_default();
            let org = parts.next().unwrap_or_default();
            let contact = protocol::SharedContact {
                name: card_name,
                phones: if phone.is_empty() { Vec::new() } else { vec![phone] },
                emails: if email.is_empty() { Vec::new() } else { vec![email] },
                organization: org,
                avatar: None,
            };
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                contacts: vec![contact],
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
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
        "group-admins" => {
            let admins: Vec<String> = words
                .filter_map(|w| match w {
                    "me" => Some(ME.to_owned()),
                    other => who(other).map(|(id, _)| id.to_owned()),
                })
                .collect();
            let club = GroupInfo {
                id: CLUB.into(),
                title: "Bridge club".into(),
                description: "Tuesdays, 7 pm".into(),
                members: 3,
                member: true,
                member_ids: vec![ME.into(), GRACE.into(), ALAN.into()],
                admins,
                member_access: GroupAccess::Member,
                attributes_access: GroupAccess::Member,
                invite_link_access: GroupAccess::Unsatisfiable,
                announcements_only: false,
                invite_link: String::new(),
                timer_secs: 0,
            };
            vec![Event::Groups(vec![club])]
        }
        "identity-change" => {
            let target = next();
            let aci = if let Some((id, _)) = who(target) {
                id.to_owned()
            } else if target == "stranger" {
                STRANGER.to_owned()
            } else if super::wire::is_aci(target) {
                target.to_owned()
            } else {
                return Vec::new();
            };
            {
                let mut map = identities.lock().unwrap_or_else(|p| p.into_inner());
                let entry = map
                    .entry(aci.clone())
                    .or_insert_with(|| FakeIdentity { key: fake_key(0x55), state: IdentityState::Default, changed: false });
                entry.key[32] = entry.key[32].wrapping_add(1);
                if entry.state == IdentityState::Verified {
                    entry.state = IdentityState::Unverified;
                    entry.changed = true;
                }
            }
            vec![Event::IdentityChanged { peer: Peer::Direct(aci) }]
        }
        "verified" => {
            let target = next();
            let aci = if let Some((id, _)) = who(target) {
                id.to_owned()
            } else if target == "stranger" {
                STRANGER.to_owned()
            } else if super::wire::is_aci(target) {
                target.to_owned()
            } else {
                return Vec::new();
            };
            let state = match next() {
                "verified" => IdentityState::Verified,
                "unverified" => IdentityState::Unverified,
                "default" => IdentityState::Default,
                _ => return Vec::new(),
            };
            {
                let mut map = identities.lock().unwrap_or_else(|p| p.into_inner());
                let entry = map
                    .entry(aci.clone())
                    .or_insert_with(|| FakeIdentity { key: fake_key(0x55), state: IdentityState::Default, changed: false });
                entry.state = state;
                entry.changed = false;
            }
            vec![Event::VerifiedChanged { aci, state }]
        }
        "pin-msg" | "unpin-msg" => {
            let where_word = next();
            let peer = if where_word == "club" {
                club()
            } else if let Some((id, _)) = who(where_word) {
                direct(id)
            } else if super::wire::is_aci(where_word) {
                direct(where_word)
            } else {
                return Vec::new();
            };
            let by_word = next();
            let by = if by_word == "me" {
                ME.to_owned()
            } else if let Some((id, _)) = who(by_word) {
                id.to_owned()
            } else if super::wire::is_aci(by_word) {
                by_word.to_owned()
            } else {
                return Vec::new();
            };
            let author_word = next();
            let author = if author_word == "me" {
                ME.to_owned()
            } else if let Some((id, _)) = who(author_word) {
                id.to_owned()
            } else if super::wire::is_aci(author_word) {
                author_word.to_owned()
            } else {
                return Vec::new();
            };
            let target = next().parse().unwrap_or(0);
            if command == "pin-msg" {
                let duration_secs = next().parse::<u32>().ok().filter(|&s| s > 0);
                vec![Event::Pinned { peer, by, author, target, duration_secs }]
            } else {
                vec![Event::Unpinned { peer, by, author, target }]
            }
        }
        "media" => {
            // Format: media WHO SENT MIME WIDTH HEIGHT FLAGS NAME URL KEY_HEX DIGEST_HEX [BLURHASH] [| CAPTION]
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let payload = rest(line, 3);
            let (meta_part, caption) = match payload.split_once('|') {
                Some((m, c)) => (m.trim(), c.trim().to_owned()),
                None => (payload.trim(), String::new()),
            };
            let mut toks = meta_part.split_whitespace();
            let mime = toks.next().unwrap_or("application/octet-stream").to_owned();
            let width = toks.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let height = toks.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let flags = toks.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let file_name = toks.next().unwrap_or("").to_owned();
            let url_or_key = toks.next().unwrap_or("").to_owned();
            let key = unhex_bytes(toks.next().unwrap_or(""));
            let digest = unhex_bytes(toks.next().unwrap_or(""));
            let blurhash = toks.next().filter(|s| *s != "-").map(str::to_owned);
            let (url, cdn_key) = if url_or_key.starts_with("http://") {
                (Some(url_or_key), None)
            } else {
                (None, Some(url_or_key))
            };
            let ptr = Pointer {
                cdn_key,
                content_type: mime,
                key,
                digest,
                file_name: if file_name == "-" { String::new() } else { file_name },
                flags,
                width,
                height,
                blurhash,
                url,
                ..Pointer::default()
            };
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                text: caption,
                attachments: vec![ptr],
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
        }
        "album" => {
            // Format: album WHO SENT MIME1:NAME1:URL1:KEY1:DIGEST1 ; MIME2:NAME2:URL2:KEY2:DIGEST2
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let spec = rest(line, 3);
            let mut attachments = Vec::new();
            for item in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                let parts: Vec<&str> = item.split('|').collect();
                let mime = parts.first().copied().unwrap_or("image/jpeg").to_owned();
                let file_name = parts.get(1).copied().unwrap_or("photo.jpg").to_owned();
                let url_or_key = parts.get(2).copied().unwrap_or("fake-photo").to_owned();
                let key = unhex_bytes(parts.get(3).copied().unwrap_or(""));
                let digest = unhex_bytes(parts.get(4).copied().unwrap_or(""));
                let (url, cdn_key) = if url_or_key.starts_with("http://") {
                    (Some(url_or_key), None)
                } else {
                    (None, Some(url_or_key))
                };
                attachments.push(Pointer {
                    cdn_key,
                    content_type: mime,
                    key,
                    digest,
                    file_name,
                    width: 400,
                    height: 300,
                    url,
                    ..Pointer::default()
                });
            }
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                attachments,
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
        }
        "quote-photo" => {
            // Format: quote-photo WHO SENT QUOTED_SENT URL KEY_HEX DIGEST_HEX | REPLY_TEXT
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let quoted = next().parse().unwrap_or(0);
            let payload = rest(line, 4);
            let (meta, reply_text) = match payload.split_once('|') {
                Some((m, r)) => (m.trim(), r.trim().to_owned()),
                None => (payload.trim(), String::new()),
            };
            let mut toks = meta.split_whitespace();
            let url_or_key = toks.next().unwrap_or("fake-photo").to_owned();
            let key = unhex_bytes(toks.next().unwrap_or(""));
            let digest = unhex_bytes(toks.next().unwrap_or(""));
            let (url, cdn_key) = if url_or_key.starts_with("http://") {
                (Some(url_or_key), None)
            } else {
                (None, Some(url_or_key))
            };
            let thumb_ptr = Pointer {
                cdn_key,
                content_type: "image/jpeg".to_owned(),
                key,
                digest,
                width: 120,
                height: 90,
                url,
                ..Pointer::default()
            };
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                text: reply_text,
                quote: Some(Quote {
                    sent: quoted,
                    author: ME.into(),
                    text: String::new(),
                    spans: Vec::new(),
                    attachments: vec![QuotedAttachment {
                        content_type: "image/jpeg".to_owned(),
                        file_name: "sunset.jpg".to_owned(),
                        thumbnail: Some(thumb_ptr),
                    }],
                }),
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
        }
        "expired-photo" => {
            let Some((author, name)) = who(next()) else {
                return Vec::new();
            };
            let sent = next().parse().unwrap_or(T0);
            let text = Text {
                author: author.into(),
                name: name.into(),
                sent,
                attachments: vec![Pointer {
                    cdn_key: Some("expired".to_owned()),
                    content_type: "image/jpeg".to_owned(),
                    file_name: "old.jpg".to_owned(),
                    width: 800,
                    height: 600,
                    ..Pointer::default()
                }],
                ..Text::default()
            };
            vec![Event::Message { peer: direct(author), text, outgoing: false }]
        }
        "drop" => vec![Event::Closed("the fake dropped the connection".into())],
        "unlinked" => vec![Event::Ended("This computer was removed from your Signal account.".into())],
        _ => Vec::new(),
    }
}

fn unhex_bytes(text: &str) -> Vec<u8> {
    if text.len() % 2 != 0 {
        return Vec::new();
    }
    (0..text.len())
        .step_by(2)
        .filter_map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
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
        let prev = |previews: &[LinkPreview]| {
            if previews.is_empty() {
                String::new()
            } else {
                let urls: Vec<&str> = previews.iter().map(|p| p.url.as_str()).collect();
                format!(" previews={}", urls.join(","))
            }
        };
        let line = match &body {
            Body::Text { text, quote: Some(quote), ranges, timer, previews } => {
                format!(
                    "send {} text {text}{}{}{} (answering {}-{})",
                    describe(to),
                    noted(ranges),
                    exp(*timer),
                    prev(previews),
                    quote.sent,
                    quote.author
                )
            }
            Body::Text { text, quote: None, ranges, timer, previews } => {
                format!("send {} text {text}{}{}{}", describe(to), noted(ranges), exp(*timer), prev(previews))
            }
            Body::Files { text, quote, ranges, files, timer } => {
                let names: Vec<String> = files.iter().map(|file| file.name.clone()).collect();
                format!("{}{}{}", super::files::call_line(&describe(to), &names, text, quote.as_ref()), noted(ranges), exp(*timer))
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
            Body::Contact { contact, timer } => format!("send {} contact {}{}", describe(to), contact.name, exp(*timer)),
            Body::Pin { author, target, duration_secs } => {
                let dur = duration_secs.map_or_else(|| "forever".to_owned(), |s| s.to_string());
                format!("send {} pin {target}-{author} dur={dur}", describe(to))
            }
            Body::Unpin { author, target } => format!("send {} unpin {target}-{author}", describe(to)),
        };
        record(&line);
        if let Peer::Direct(aci) = to {
            let blocked = self
                .identities
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(aci)
                .is_some_and(|id| id.changed);
            if blocked {
                return Err(Fault::Refused(
                    "Your safety number with this contact has changed. Review and approve their safety number before sending.".to_owned(),
                ));
            }
        }
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

    fn create_group(&self, title: &str, members: &[String]) -> Result<GroupInfo, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        record(&format!("create-group {title} | {}", members.join(",")));
        if let Some(words) = self.refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        let seq = self.clock.fetch_add(1, Ordering::Relaxed);
        let id = format!("{seq:064x}");
        let mut member_ids = vec![ME.to_owned()];
        for aci in members {
            if !member_ids.contains(aci) {
                member_ids.push(aci.clone());
            }
        }
        Ok(GroupInfo {
            id,
            title: title.to_owned(),
            description: String::new(),
            members: member_ids.len() as u32,
            member: true,
            member_ids,
            admins: vec![ME.to_owned()],
            member_access: GroupAccess::Member,
            attributes_access: GroupAccess::Member,
            invite_link_access: GroupAccess::Unsatisfiable,
            announcements_only: false,
            invite_link: String::new(),
            timer_secs: 0,
        })
    }

    fn leave_group(&self, group_id: &str) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        record(&format!("leave-group {group_id}"));
        if let Some(words) = self.refuse.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            return Err(Fault::Refused(words));
        }
        Ok(())
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

    fn safety_number(&self, aci: &str) -> Result<Option<SafetyNumber>, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let map = self.identities.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = map.get(aci) else {
            return Ok(None);
        };
        let me_key = fake_key(0x11);
        let Some(digits) = super::wire::safety_number_for_acis(ME, &me_key, aci, &entry.key) else {
            return Ok(None);
        };
        Ok(Some(SafetyNumber {
            digits,
            state: entry.state,
            changed: entry.changed,
        }))
    }

    fn set_verified(&self, aci: &str, state: IdentityState) -> Result<(), Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        {
            let mut map = self.identities.lock().unwrap_or_else(|p| p.into_inner());
            let entry = map
                .entry(aci.to_owned())
                .or_insert_with(|| FakeIdentity { key: fake_key(0x55), state: IdentityState::Default, changed: false });
            entry.state = state;
            entry.changed = false;
        }
        let label = match state {
            IdentityState::Default => "default",
            IdentityState::Verified => "verified",
            IdentityState::Unverified => "unverified",
        };
        record(&format!("verify {aci} {label}"));
        let _ = self.events.send(Event::VerifiedChanged { aci: aci.to_owned(), state });
        Ok(())
    }

    fn person_profile(&self, aci: &str) -> Result<PersonProfile, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        record(&format!("person-profile {aci}"));
        let found = self
            .profiles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(aci)
            .cloned()
            .unwrap_or(PersonProfile { key_known: false, ..PersonProfile::default() });
        Ok(found)
    }

    fn fetch_link_preview(&self, url: &str) -> Result<Option<LinkPreview>, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        record(&format!("preview-fetch {url}"));
        let host = crate::adapters::tiny::preview::real_host(url);
        if host.is_empty() {
            return Ok(None);
        }
        Ok(Some(LinkPreview {
            url: url.to_owned(),
            title: format!("Preview for {host}"),
            description: "Fetched via Signal content proxy".to_owned(),
            date: 0,
            image: None,
            image_width: 0,
            image_height: 0,
        }))
    }

    fn download_attachment(&self, pointer: &Pointer, dest: &std::path::Path) -> Result<u64, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        if let Some(url) = pointer.url.as_deref() {
            if !(url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")) {
                return Err(Fault::Refused("Invalid local CDN stub URL.".into()));
            }
            record(&format!("attachment-download {url}"));
            let config = ureq::Agent::config_builder().http_status_as_error(false).build();
            let agent = ureq::Agent::new_with_config(config);
            let resp = agent.get(url).call().map_err(|err| Fault::Net(err.to_string()))?;
            let status = resp.status().as_u16();
            if matches!(status, 403 | 404 | 410) {
                return Err(Fault::Refused("No longer available".to_owned()));
            }
            if !(200..300).contains(&status) {
                return Err(Fault::Net(format!("CDN returned {status}")));
            }
            let reader = resp.into_body().into_reader();
            return super::files::decrypt_attachment_stream(reader, &pointer.key, &pointer.digest, pointer.size, dest)
                .map_err(Fault::Refused);
        }
        match pointer.cdn_key.as_deref() {
            Some("expired") => {
                record("attachment-download expired");
                Err(Fault::Refused("No longer available".to_owned()))
            }
            Some(key) => {
                record(&format!("attachment-download {key}"));
                let bytes: Vec<u8> = if pointer.flags & FLAG_GIF != 0 || pointer.content_type == "image/gif" {
                    b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;".to_vec()
                } else if pointer.content_type.starts_with("image/") {
                    // Valid 1x1 PNG
                    const PNG: &str = "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000c4944415408d763f8cfc0000000020001e221bc330000000049454e44ae426082";
                    unhex_bytes(PNG)
                } else {
                    b"%PDF-1.4\nfake-file\n".to_vec()
                };
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(dest, &bytes).map_err(|err| Fault::Net(err.to_string()))?;
                Ok(bytes.len() as u64)
            }
            None => Err(Fault::Refused("No longer available".to_owned())),
        }
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
