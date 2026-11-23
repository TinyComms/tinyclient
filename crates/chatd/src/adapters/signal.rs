//! Signal: text, photos, videos, files, and end-to-end encrypted 1:1 and group calls. This
//! computer is linked to the user's account as one more device, the way Signal Desktop is; it
//! never registers a phone number, which would take the account from the phone. Call signalling
//! runs over Signal's encrypted `CallMessage` path here, while `mediad` carries WebRTC media
//! (pure-Rust `str0m` Opus + H.264 over DTLS-SRTP).
//!
//! # The library: presage, in this process (decided 2026-10-14, by building it on this Mac)
//!
//! presage (whisperfish, on libsignal-service-rs and Signal's own libsignal) is the maintained
//! Rust client; the alternatives are signal-cli, a JVM daemon of a few hundred megabytes resident,
//! and Signal Desktop's own code, which is Electron. presage builds here once `protoc` is on the
//! path (Signal's post-quantum ratchet compiles its protobufs at build time); with its `cdsi`
//! feature off nothing needs CMake or BoringSSL. The `signal` cargo feature is on by default.
//! presage, libsignal-service-rs and libsignal are AGPL-3.0, so a chatd that links them is a
//! combined work under the AGPL. libsignal-service speaks HTTPS and websockets through reqwest
//! and rustls (with ring), pinned to Signal's own certificate authority; the rest of chatd uses
//! the system's TLS. presage and libsignal-service are git dependencies at fixed commits.
//!
//! Like Telegram's grammers it is async: one current-thread tokio runtime, on the adapter's own
//! thread (`real`), built when an account exists or a link begins and at no other time.
//!
//! # The session
//!
//! The keychain item holds who the account is and the key to its state (`Saved`). The state
//! itself, which Signal's ratchets change with every message (identity and prekeys, sessions,
//! group keys, contacts), is one file per account under `signal/` in the support directory, each
//! value sealed with that key (`vault`). No message is kept there: messages go to chatd's store
//! as they arrive, and when linking a new device the primary phone may transfer encrypted message
//! history (chats and recent media pointers) into chatd's store (`transfer`). Signal's servers do
//! not retain older history for later on-demand backfill.
//!
//! # What the user did, and only that
//!
//! Read receipts go out when the shell says a message was read and the user's own Signal settings
//! send them; typing likewise. Both stay off until the phone has said what those settings are.
//! Nothing else is sent unasked: no reaction, edit, delete or profile change.

mod fake;
mod files;
#[cfg(feature = "signal")]
mod real;
mod style;
pub mod signin;
pub mod transfer;
#[cfg(feature = "signal")]
mod vault;
mod wire;

use super::{keep_connected, serve_commands, About, CatalogPage, Command, Context, ConversationAbout, Failure, Members, Picture, Refusal, SharedPage, Signed};
use crate::blobs::upload::{self, Upload};
use crate::session;
use crate::store::{ConversationRow, Filing, FolderRow, PersonDetails, PersonRow};
use protocol::{
    Attachment, CallFailure, CallRing, CallSecurity, CallState, CatalogItem, CatalogKind, Contact, ContactKind,
    ConversationKind, ConversationRights, Delivery, Link, Mark, MediaFile, MediaKind, MemberSection, Message,
    MessageKind, Occupant, Outgoing, Participant, Person, Poll, PollBallot, PollDraft, Reaction, RelateAction,
    Relation, ReplyTo, ReportReason, ReportTarget, SecureMember, SecurityMode, SignalAccept, SignalOffer,
    SignalRelay, Span, Special, Trust,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wire::{
    decode_group_call_signal, encode_group_call_signal, view_once_label, Body, CallSignal, Event, Fault,
    GroupCallInfo, GroupCallSignal, GroupRingMode, HangupKind, Peer, Quote, Text, Wire,
};

/// Signal has no on-demand server history backfill, so nothing is paced (linked-device history
/// transfer runs once during linking in `signin`).
const PACE: Duration = Duration::ZERO;
/// Others are told the user is typing at most this often per conversation. Signal shows it for
/// fifteen seconds and its own apps renew it every ten.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// How long "is typing" stays without another signal.
const TYPING_SHOWN: i64 = 15;
/// Unread messages remembered per conversation, for the receipts a read sends.
const UNREAD_KEPT: usize = 500;
/// Conversations with unread messages remembered.
const CONVERSATIONS_KEPT: usize = 1_024;
/// An offer older than this is catch-up from a call that is long over: a line to record when
/// it ends, not news to ring.
const CALL_STALE: i64 = 120;
/// Our sent messages remembered, for the receipt that says where one was read.
const SENT_KEPT: usize = 1_024;
/// What a message says when this account does not share read receipts.
const RECEIPTS_OFF: &str = "Read receipts are off";
/// Reactions remembered, so a new one from the same person replaces their old one.
const REACTIONS_KEPT: usize = 4_096;
/// Largest reaction record file read on startup. `REACTIONS_KEPT` rows sit well below this;
/// anything larger is skipped so an oversized or corrupt file is not parsed under the lock.
const REACTIONS_FILE_LIMIT: u64 = 2 * 1024 * 1024;
/// Ballots remembered on one poll. Older ones drop off the list; the choice counts stay.
const BALLOTS_KEPT: usize = 1_024;
/// Signal's poll limits, from its desktop schema: a question and each choice are at most this
/// many characters, and a poll has at most this many choices.
const POLL_CHARS: usize = 100;
const POLL_MOST: usize = 10;
/// A vote or an end, packed into an edit so the command channel already exists. A real edit
/// never starts with this mark.
const POLL_VOTE: &str = "\u{1}signal-poll-vote\u{1}";
const POLL_END: &str = "\u{1}signal-poll-end\u{1}";
/// Promote or revoke a group admin, packed into a `MembersRemove` person string (`\u{1}admin\u{1}<aci>`
/// or `\u{1}member\u{1}<aci>`). An ACI never starts with `\u{1}`.
const GROUP_ADMIN_PROMOTE: &str = "\u{1}signal-admin-promote\u{1}";
const GROUP_ADMIN_REVOKE: &str = "\u{1}signal-admin-revoke\u{1}";
/// Group permissions edit packed into `ConversationUpdate.description`:
/// `\u{1}signal-group-perms\u{1}members=admin|member;attributes=admin|member;announcements=on|off`.
const GROUP_PERMS_PREFIX: &str = "\u{1}signal-group-perms\u{1}";
/// Group invite link control packed into `ConversationUpdate.description`:
/// `\u{1}signal-group-link\u{1}off|on|approval|reset`.
const GROUP_LINK_PREFIX: &str = "\u{1}signal-group-link\u{1}";
/// Join request approval/denial packed into `MembersRemove.person`:
/// `\u{1}signal-join-approve\u{1}<aci>` or `\u{1}signal-join-deny\u{1}<aci>`.
const GROUP_JOIN_APPROVE: &str = "\u{1}signal-join-approve\u{1}";
const GROUP_JOIN_DENY: &str = "\u{1}signal-join-deny\u{1}";
/// Join a group from its `https://signal.group/#...` link packed into `StartDirect.person_id`.
const GROUP_JOIN_LINK: &str = "\u{1}signal-join-link\u{1}";
/// Signal call link commands packed into `Handoff.conversation_id`:
/// - `\u{1}signal-call-link-create\u{1}<approve:0|1>|<name>` -> returns `https://signal.link/call/#key=...`
/// - `\u{1}signal-call-link-update\u{1}<approve:0|1>|<url>|<name>` -> returns updated `https://signal.link/call/#key=...`
/// - `\u{1}signal-call-link-join\u{1}<url>` -> returns `room:<hex>` or `waiting:<hex>`
/// - `\u{1}signal-call-link-delete\u{1}<url>` -> returns `"deleted"`
const CALL_LINK_CREATE: &str = "\u{1}signal-call-link-create\u{1}";
const CALL_LINK_UPDATE: &str = "\u{1}signal-call-link-update\u{1}";
const CALL_LINK_JOIN: &str = "\u{1}signal-call-link-join\u{1}";
const CALL_LINK_DELETE: &str = "\u{1}signal-call-link-delete\u{1}";
/// Signal safety-number actions packed into `Handoff.conversation_id`:
/// - `\u{1}signal-safety-verify\u{1}<aci>` -> mark contact Verified, returns formatted 60-digit safety number
/// - `\u{1}signal-safety-approve\u{1}<aci>` -> approve changed key (Default), clears send block
/// - `\u{1}signal-safety-unverify\u{1}<aci>` -> clear Verified mark back to Default
const SAFETY_VERIFY: &str = "\u{1}signal-safety-verify\u{1}";
const SAFETY_APPROVE: &str = "\u{1}signal-safety-approve\u{1}";
const SAFETY_UNVERIFY: &str = "\u{1}signal-safety-unverify\u{1}";
/// Signal sticker-pack actions packed into `Handoff.conversation_id`:
/// - `\u{1}signal-sticker-preview\u{1}<url_or_id|key>` -> returns JSON of `StickerPackInfo`
/// - `\u{1}signal-sticker-install\u{1}<url_or_id|key>` -> installs & syncs pack, returns JSON of `StickerPackInfo`
/// - `\u{1}signal-sticker-remove\u{1}<pack_id>` -> removes & syncs pack, returns `"removed"`
const STICKER_PACK_PREVIEW: &str = "\u{1}signal-sticker-preview\u{1}";
const STICKER_PACK_INSTALL: &str = "\u{1}signal-sticker-install\u{1}";
const STICKER_PACK_REMOVE: &str = "\u{1}signal-sticker-remove\u{1}";

/// Dedicated conversation id holding active Signal Stories (24h lifetime).
pub const STORIES_CONVERSATION: &str = "stories";
/// Signal Stories expire 24 hours after they were posted.
const STORY_LIFETIME_SECS: i64 = 86_400;
/// Signal Story actions packed into `Handoff.conversation_id`:
const STORY_VIEW: &str = "\u{1}signal-story-view\u{1}";
const STORY_REPLY: &str = "\u{1}signal-story-reply\u{1}";
const STORY_REACT: &str = "\u{1}signal-story-react\u{1}";
const STORY_POST: &str = "\u{1}signal-story-post\u{1}";
const STORY_TOGGLE: &str = "\u{1}signal-story-toggle\u{1}";

/// Packs a Signal story view request (marks viewed & sends `Viewed` receipt when enabled) for `Request::Handoff`.
pub fn story_view_command(author: &str, sent: u64) -> String {
    format!("{STORY_VIEW}{author}|{sent}")
}

/// Packs a Signal story reply (`DataMessage.story_context`) for `Request::Handoff`.
pub fn story_reply_command(author: &str, sent: u64, group_id: Option<&str>, text: &str) -> String {
    format!("{STORY_REPLY}{author}|{sent}|{}|{text}", group_id.unwrap_or(""))
}

/// Packs a Signal story reaction (`DataMessage.reaction` + `story_context`) for `Request::Handoff`.
pub fn story_react_command(author: &str, sent: u64, group_id: Option<&str>, emoji: &str) -> String {
    format!("{STORY_REACT}{author}|{sent}|{}|{emoji}", group_id.unwrap_or(""))
}

/// Packs a Signal story post (distribution list id/name or `g.<hex>` group) for `Request::Handoff`.
pub fn story_post_command(target: &str, allows_replies: bool, bg_hex: &str, text: &str) -> String {
    format!("{STORY_POST}{target}|{}|{bg_hex}|{text}", u8::from(allows_replies))
}

/// Packs a Signal stories on/off toggle for `Request::Handoff`.
pub fn story_toggle_command(enabled: bool) -> String {
    format!("{STORY_TOGGLE}{}", if enabled { "on" } else { "off" })
}

/// Packs a Signal sticker-pack preview request for `Request::Handoff`.
pub fn sticker_pack_preview_command(url_or_pair: &str) -> String {
    format!("{STICKER_PACK_PREVIEW}{url_or_pair}")
}

/// Packs a Signal sticker-pack install request for `Request::Handoff`.
pub fn sticker_pack_install_command(url_or_pair: &str) -> String {
    format!("{STICKER_PACK_INSTALL}{url_or_pair}")
}

/// Packs a Signal sticker-pack remove request for `Request::Handoff`.
pub fn sticker_pack_remove_command(pack_id: &str) -> String {
    format!("{STICKER_PACK_REMOVE}{pack_id}")
}

/// Packs a Signal safety-number verify request for `Request::Handoff`.
pub fn safety_verify_command(aci: &str) -> String {
    format!("{SAFETY_VERIFY}{aci}")
}

/// Packs a Signal safety-number approve (clear changed-key send block) request for `Request::Handoff`.
pub fn safety_approve_command(aci: &str) -> String {
    format!("{SAFETY_APPROVE}{aci}")
}

/// Packs a Signal safety-number unverify request for `Request::Handoff`.
pub fn safety_unverify_command(aci: &str) -> String {
    format!("{SAFETY_UNVERIFY}{aci}")
}

/// Packs a Signal call link creation request for `Request::Handoff`.
pub fn call_link_create_command(name: &str, approve_all: bool) -> String {
    format!("{CALL_LINK_CREATE}{}|{name}", u8::from(approve_all))
}

/// Packs a Signal call link join request for `Request::Handoff`.
pub fn call_link_join_command(url: &str) -> String {
    format!("{CALL_LINK_JOIN}{url}")
}

/// Packs a Signal call link delete request for `Request::Handoff`.
pub fn call_link_delete_command(url: &str) -> String {
    format!("{CALL_LINK_DELETE}{url}")
}

/// Packs a Signal group invite link mode change (`off`, `on`, `approval`, `reset`).
pub fn group_link_command(mode: &str) -> String {
    format!("{GROUP_LINK_PREFIX}{mode}")
}

/// Packs a join-request approval or denial for `MembersRemove`.
pub fn group_join_request_command(person: &str, approve: bool) -> String {
    if approve {
        format!("{GROUP_JOIN_APPROVE}{person}")
    } else {
        format!("{GROUP_JOIN_DENY}{person}")
    }
}

/// Packs a `https://signal.group/#...` join request for `StartDirect`.
pub fn group_join_link_command(url: &str) -> String {
    format!("{GROUP_JOIN_LINK}{url}")
}

/// Packs a Signal group permissions edit so `Request::ConversationUpdate` carries it.
pub fn group_permissions_command(
    member_access: Option<bool>,
    attributes_access: Option<bool>,
    announcements_only: Option<bool>,
) -> String {
    let mut parts = Vec::new();
    if let Some(admins_only) = member_access {
        parts.push(format!("members={}", if admins_only { "admin" } else { "member" }));
    }
    if let Some(admins_only) = attributes_access {
        parts.push(format!("attributes={}", if admins_only { "admin" } else { "member" }));
    }
    if let Some(on) = announcements_only {
        parts.push(format!("announcements={}", if on { "on" } else { "off" }));
    }
    format!("{GROUP_PERMS_PREFIX}{}", parts.join(";"))
}

fn parse_group_permissions(text: &str) -> Option<wire::GroupEdit> {
    let rest = text.strip_prefix(GROUP_PERMS_PREFIX)?;
    let mut edit = wire::GroupEdit::default();
    for pair in rest.split(';').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=')?;
        match (k, v) {
            ("members", "admin") => edit.member_access = Some(wire::GroupAccess::Administrator),
            ("members", "member") => edit.member_access = Some(wire::GroupAccess::Member),
            ("attributes", "admin") => edit.attributes_access = Some(wire::GroupAccess::Administrator),
            ("attributes", "member") => edit.attributes_access = Some(wire::GroupAccess::Member),
            ("announcements", "on") => edit.announcements_only = Some(true),
            ("announcements", "off") => edit.announcements_only = Some(false),
            _ => return None,
        }
    }
    Some(edit)
}

/// Packs a Signal group admin role change so the existing member-management channel carries it.
pub fn group_admin_command(person: &str, admin: bool) -> String {
    if admin {
        format!("{GROUP_ADMIN_PROMOTE}{person}")
    } else {
        format!("{GROUP_ADMIN_REVOKE}{person}")
    }
}

fn group_admin_action(person: &str) -> Option<(&str, bool)> {
    if let Some(aci) = person.strip_prefix(GROUP_ADMIN_PROMOTE) {
        Some((aci, true))
    } else if let Some(aci) = person.strip_prefix(GROUP_ADMIN_REVOKE) {
        Some((aci, false))
    } else {
        None
    }
}
/// What a person with no name we know is called.
const NO_NAME: &str = "Signal user";

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// One member, as a group change compares them. `admin` is Signal's administrator role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSnap {
    pub id: String,
    pub admin: bool,
}

/// The parts of a group a change can show. Built from the stored group, never from an unsigned change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupSnap {
    pub title: String,
    pub description: String,
    pub timer_secs: u32,
    pub avatar: String,
    pub members: Vec<MemberSnap>,
    pub pending: Vec<String>,
    pub requesting: Vec<String>,
    /// Anyone with the link may join.
    pub link: bool,
}

/// What changed between two snapshots of one group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupDelta {
    pub lines: Vec<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub left: bool,
    pub rejoined: bool,
}

/// Tracks the last revision applied for each group. The first snapshot is recorded silently;
/// stale or duplicate revisions are skipped; a newer revision yields its prior snapshot once.
#[derive(Debug, Default)]
pub struct GroupRevisions {
    known: HashMap<String, (u32, GroupSnap)>,
}

impl GroupRevisions {
    pub fn contains(&self, id: &str) -> bool {
        self.known.contains_key(id)
    }

    /// Records the initial snapshot for a group without announcing a change.
    /// Does not overwrite an already-tracked group.
    pub fn seed(&mut self, id: String, revision: u32, snap: GroupSnap) {
        self.known.entry(id).or_insert((revision, snap));
    }

    /// True only when `id` is tracked and `revision` is strictly newer than the last applied one.
    pub fn should_check(&self, id: &str, revision: u32) -> bool {
        self.known.get(id).is_some_and(|(known_rev, _)| revision > *known_rev)
    }

    /// Advances `id` to `stored_revision` and returns the prior snapshot if `stored_revision`
    /// is strictly newer than the last applied one.
    pub fn advance(&mut self, id: &str, stored_revision: u32, after: GroupSnap) -> Option<GroupSnap> {
        let known_rev = self.known.get(id)?.0;
        if stored_revision <= known_rev {
            return None;
        }
        let (_, before) = self.known.insert(id.to_owned(), (stored_revision, after))?;
        Some(before)
    }
}

fn person(id: &str, me: &str, name: &impl Fn(&str) -> String) -> String {
    if id == me {
        "You".to_owned()
    } else {
        let named = name(id);
        if named.is_empty() { "Someone".to_owned() } else { named }
    }
}

fn duration(secs: u32) -> String {
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        let days = secs / 86_400;
        format!("{days} day{}", if days == 1 { "" } else { "s" })
    } else if secs >= 3_600 && secs.is_multiple_of(3_600) {
        let hours = secs / 3_600;
        format!("{hours} hour{}", if hours == 1 { "" } else { "s" })
    } else if secs >= 60 && secs.is_multiple_of(60) {
        let minutes = secs / 60;
        format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
    } else {
        format!("{secs} seconds")
    }
}

/// The lines and the row fields for one group revision. The first snapshot of a connection is not
/// passed here: a connect has no "before", so it shows the current name and no system lines.
pub fn group_delta(before: &GroupSnap, after: &GroupSnap, me: &str, name: impl Fn(&str) -> String) -> GroupDelta {
    let mut lines = Vec::new();
    let title = (before.title != after.title).then(|| {
        lines.push(format!("The group name is now {}", after.title));
        after.title.clone()
    });
    let description = (before.description != after.description).then(|| {
        if after.description.is_empty() {
            lines.push("The group description was cleared".to_owned());
        } else {
            lines.push(format!("The group description is now {}", after.description));
        }
        after.description.clone()
    });
    if before.avatar != after.avatar {
        lines.push("The group photo changed".to_owned());
    }
    if before.timer_secs != after.timer_secs {
        if after.timer_secs == 0 {
            lines.push("Disappearing messages are off".to_owned());
        } else {
            lines.push(format!("Disappearing messages are set to {}", duration(after.timer_secs)));
        }
    }
    if before.link != after.link {
        lines.push(if after.link { "The group link is on".to_owned() } else { "The group link is off".to_owned() });
    }
    let was: HashMap<&str, bool> = before.members.iter().map(|member| (member.id.as_str(), member.admin)).collect();
    let now: HashMap<&str, bool> = after.members.iter().map(|member| (member.id.as_str(), member.admin)).collect();
    let mut joined: Vec<&str> = now.keys().copied().filter(|id| !was.contains_key(id)).collect();
    joined.sort_unstable();
    let mut rejoined = false;
    for id in joined {
        if id == me {
            rejoined = true;
        }
        lines.push(format!("{} joined the group", person(id, me, &name)));
    }
    let mut stayed: Vec<&str> = now.keys().copied().filter(|id| was.get(id).is_some_and(|admin| *admin != now[id])).collect();
    stayed.sort_unstable();
    for id in stayed {
        let admin = now[id];
        let line = if id == me {
            if admin { "You are now an admin".to_owned() } else { "You are no longer an admin".to_owned() }
        } else if admin {
            format!("{} is now an admin", person(id, me, &name))
        } else {
            format!("{} is no longer an admin", person(id, me, &name))
        };
        lines.push(line);
    }
    let mut gone: Vec<&str> = was.keys().copied().filter(|id| !now.contains_key(id)).collect();
    gone.sort_unstable();
    let mut left = false;
    for id in gone {
        if id == me {
            left = true;
            lines.push("You are no longer in this group".to_owned());
        } else {
            lines.push(format!("{} is no longer in the group", person(id, me, &name)));
        }
    }
    let pending: HashSet<&str> = before.pending.iter().map(String::as_str).collect();
    let mut invited: Vec<&str> = after.pending.iter().map(String::as_str).filter(|id| !pending.contains(id) && !now.contains_key(id)).collect();
    invited.sort_unstable();
    for id in invited {
        lines.push(format!("{} was invited", person(id, me, &name)));
    }
    let asking: HashSet<&str> = before.requesting.iter().map(String::as_str).collect();
    let mut requests: Vec<&str> = after.requesting.iter().map(String::as_str).filter(|id| !asking.contains(id)).collect();
    requests.sort_unstable();
    for id in requests {
        lines.push(format!("{} asked to join", person(id, me, &name)));
    }
    GroupDelta { lines, title, description, left, rejoined }
}

/// The lab driver for `signal-group-rename`. A build that drops a rename fails.
pub fn prove_signal_group_rename() -> Result<(), String> {
    let before = GroupSnap { title: "Bridge club".into(), ..GroupSnap::default() };
    let after = GroupSnap { title: "Wednesday club".into(), ..GroupSnap::default() };
    let delta = group_delta(&before, &after, "me", |_| String::new());
    if delta.title.as_deref() != Some("Wednesday club") || delta.lines != ["The group name is now Wednesday club"] || delta.left {
        return Err("a renamed group must take the new name and one line".into());
    }
    let member = GroupSnap { members: vec![MemberSnap { id: "me".into(), admin: false }], ..GroupSnap::default() };
    let left = group_delta(&member, &GroupSnap::default(), "me", |_| String::new());
    if !left.left || left.lines != ["You are no longer in this group"] {
        return Err("leaving the group must say so".into());
    }
    Ok(())
}

/// Most pictures asked of Signal for one change to what is on screen.
const PICTURES: usize = 25;
/// What the official apps keep in the message itself of a long text; more travels as Signal's
/// own plain-text attachment, whose first part the body repeats. Sending that way is still to
/// come. The composer counts in characters where Signal cuts in bytes on a character boundary.
const TEXT_LIMIT: u32 = 2_048;
/// Marks a `SetPresence` status that is really an edit of our own profile. A status a person
/// typed never starts with this, and the server sends it only for Signal, without changing
/// presence.
const PROFILE_MARK: char = '\u{1}';
const PROFILE_PREFIX: &str = "\u{1}signal-profile\u{1}";
/// A username still needs a reservation this library does not make.
pub const USERNAME_ON_PHONE: &str = "A Signal username is still changed on your phone.";
/// Uploading an avatar needs a cipher stream this library leaves unimplemented.
pub const PHOTO_ON_PHONE: &str = "A Signal profile photo is still changed on your phone.";

/// Packs a profile edit into the status a `SetPresence` already carries. `None` leaves that
/// part. Lengths are Signal's own, cut here so a long paste cannot become the whole command.
pub fn profile_status(name: Option<&str>, about: Option<&str>, emoji: Option<&str>) -> String {
    format!("{PROFILE_PREFIX}{}{PROFILE_MARK}{}{PROFILE_MARK}{}", profile_field(name, 64), profile_field(about, 350), profile_field(emoji, 16))
}

fn profile_field(value: Option<&str>, limit: usize) -> String {
    match value {
        None => "0".to_owned(),
        Some(text) => {
            let text: String = text.chars().filter(|c| *c != PROFILE_MARK).take(limit).collect();
            format!("1{text}")
        }
    }
}

/// The name, about, and emoji in a status [`profile_status`] packed. `None` when it is a
/// real status, or a field that was left alone.
pub fn profile_edit(status: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let rest = status.strip_prefix(PROFILE_PREFIX)?;
    let mut parts = rest.split(PROFILE_MARK);
    Some((profile_part(parts.next()?)?, profile_part(parts.next()?)?, profile_part(parts.next()?)?))
}

fn profile_part(part: &str) -> Option<Option<String>> {
    let mut chars = part.chars();
    match chars.next() {
        Some('0') => Some(None),
        Some('1') => Some(Some(chars.collect())),
        _ => None,
    }
}

/// The lab's signal-long-text driver. A build without the Signal library has no driver, so it stays up and the lab fails.
pub fn prove_signal_long_text() -> Result<(), String> {
    #[cfg(feature = "signal")]
    {
        real::prove_long_text()
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// The lab's signal-own-profile driver. A build without this stays up, so the driver fails.
pub fn prove_signal_own_profile() -> Result<(), String> {
    let packed = profile_status(Some("Ada"), Some("New about"), None);
    let (name, about, emoji) = profile_edit(&packed).ok_or("a profile edit was not recognised")?;
    if name.as_deref() != Some("Ada") || about.as_deref() != Some("New about") || emoji.is_some() {
        return Err(format!("the profile edit did not round-trip: {name:?} {about:?} {emoji:?}"));
    }
    if profile_edit("Lunch").is_some() {
        return Err("a status was read as a profile edit".into());
    }
    if !USERNAME_ON_PHONE.to_lowercase().contains("phone") || !PHOTO_ON_PHONE.to_lowercase().contains("phone") {
        return Err("username and photo are still changed on the phone".into());
    }
    Ok(())
}

/// The lab's signal-payment-labels driver. A build without this stays up, so the driver fails.
pub fn prove_signal_payment_labels() -> Result<(), String> {
    #[cfg(feature = "signal")]
    {
        real::prove_payment_labels()
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// The lab's signal-view-once driver. A build without this stays up, so the driver fails.
pub fn prove_signal_view_once() -> Result<(), String> {
    let wants = [(Some("image/jpeg"), "View-once photo · open it on your phone"), (Some("video/mp4"), "View-once video · open it on your phone"), (None, "View-once media · open it on your phone")];
    for (kind, want) in wants {
        let said = view_once_label(kind);
        if said != want {
            return Err(format!("a view-once message is labelled {said:?}, not {want:?}"));
        }
    }
    Ok(())
}

/// The lab's signal-privacy-local driver. Signal takes a local switch for typing and read
/// receipts: stored in its account scope, on until chosen, off stops it here. Nothing stored is
/// the account's own setting, which stays off until the phone says otherwise.
pub fn prove_signal_privacy_local() -> Result<(), String> {
    let account = protocol::AccountKey { network: protocol::Network::Signal, account: "prove".to_owned() };
    let scope = protocol::settings::account_scope(&account);
    if !protocol::settings::typing_choice(protocol::Network::Signal) {
        return Err("Signal has no local typing switch".to_owned());
    }
    if !protocol::settings::valid(&scope, protocol::settings::TYPING, "off") || !protocol::settings::valid(&scope, protocol::settings::RECEIPTS, "off") {
        return Err("Signal's account scope takes no local off for typing or receipts".to_owned());
    }
    let mut values = protocol::settings::Values::default();
    if !values.typing(&account) || !values.receipts(&account) {
        return Err("nothing stored must leave both on".to_owned());
    }
    values.set(&scope, protocol::settings::TYPING, Some("off".into()));
    values.set(&scope, protocol::settings::RECEIPTS, Some("off".into()));
    if values.typing(&account) || values.receipts(&account) {
        return Err("off here did not stop it".to_owned());
    }
    Ok(())
}

/// One day, in milliseconds. Signal Desktop still sends an edit or a delete for everyone for this
/// long (`DAY` in its source) and then does not.
const SEND_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// Two days. An incoming edit or delete for everyone older than this is ignored (`DAY * 2`).
const RECEIVE_WINDOW_MS: u64 = 2 * SEND_WINDOW_MS;

/// A test in a scratch directory asked for the scripted wire.
fn scripted() -> bool {
    !session::enabled() && std::env::var_os("TINYCLIENT_SIGNAL_FAKE").is_some()
}

pub fn supported() -> bool {
    cfg!(feature = "signal") || scripted()
}

/// Why Signal cannot be used in this build, for the sign-in sheet.
pub fn unsupported_reason() -> String {
    "This build of tinyclient was made without Signal, so it cannot sign in to Signal.".to_owned()
}

/// What the keychain holds for Signal. Not `Debug`: `vault` opens the account's keys.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    /// The account id (ACI).
    pub aci: String,
    #[serde(default)]
    pub name: String,
    /// The key the account's state file is sealed with, hex.
    #[serde(default)]
    pub vault: String,
}

fn parse(session: &str) -> Result<Saved, String> {
    let saved: Saved = serde_json::from_str(session).map_err(|_| "the Signal session is unreadable".to_owned())?;
    if !wire::is_aci(&saved.aci) {
        return Err("the Signal session names no account".to_owned());
    }
    Ok(saved)
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let saved = parse(session)?;
    Ok(vec![Signed {
        account: saved.aci.clone(),
        name: if saved.name.is_empty() { "Signal".to_owned() } else { saved.name },
        session: session.to_owned(),
    }])
}

/// One link is one account, so nothing is left once it goes.
pub fn without(_session: &str, _account: &str) -> Option<String> {
    None
}

/// A wire for this account.
fn open(saved: &Saved) -> Result<Box<dyn Wire>, String> {
    if scripted() {
        return Ok(Box::new(fake::Fake::open(saved)));
    }
    #[cfg(feature = "signal")]
    {
        Ok(Box::new(real::Real::open(saved)?))
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// What links a new device, for the sign-in.
fn linker() -> Result<Box<dyn wire::Linker>, String> {
    if scripted() {
        return Ok(Box::new(fake::FakeLinker));
    }
    #[cfg(feature = "signal")]
    {
        Ok(Box::new(real::RealLinker))
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// A thread that runs presage: the wire's, or a link's. Unoptimized, presage and libsignal keep far
/// more on the stack than Rust's default 2 MiB, and a debug build overflows it, which aborts chatd.
/// Only the pages a thread touches use memory.
fn presage_thread(name: &str) -> std::thread::Builder {
    let thread = std::thread::Builder::new().name(name.to_owned());
    if cfg!(debug_assertions) {
        thread.stack_size(64 * 1024 * 1024)
    } else {
        thread
    }
}

/// What this device kept of an account, gone: after unlinking, or when signing out offline.
fn forget(saved: &Saved) {
    #[cfg(feature = "signal")]
    if !scripted() {
        real::forget(saved);
    }
    let _ = saved;
}

/// Small things remembered while running. All bounded.
#[derive(Default)]
struct Live {
    /// Names as the user knows people: contact names, else what their messages said.
    names: HashMap<String, String>,
    /// Contact nicknames from Signal's synced storage (preferred over profile names in rows).
    nicknames: HashMap<String, String>,
    /// Private notes on contacts from Signal's synced storage.
    notes: HashMap<String, String>,
    /// Phone numbers shared by contacts (from contact sync or synced storage).
    phones: HashMap<String, String>,
    /// Stored Signal usernames (from username search or synced storage).
    usernames: HashMap<String, String>,
    /// Universal default disappearing-messages timer in seconds from AccountRecord (0 = off).
    default_timer: u32,
    /// Account's "Generate link previews" setting (from configuration sync or AccountRecord).
    link_previews: bool,
    /// Per-contact identity verification state (`Default`, `Verified`, `Unverified`).
    identity_states: HashMap<String, wire::IdentityState>,
    /// Verified contacts whose identity key changed and has not yet been approved.
    identity_changed: HashSet<String>,
    /// Unread incoming messages per conversation, (author, sent), oldest first.
    unread: HashMap<String, VecDeque<(String, u64)>>,
    /// Our sent messages and their conversation, for receipts that name only the time.
    sent: VecDeque<(u64, String)>,
    /// A group's members, including us, keyed by the conversation id. The read tick waits
    /// until every member but us has read.
    groups: HashMap<String, Vec<String>>,
    /// A group's administrator ACIs, keyed by the conversation id.
    group_admins: HashMap<String, Vec<String>>,
    /// Full group metadata, keyed by conversation id (`g.<hex>`), for info panes and access checks.
    group_info: HashMap<String, wire::GroupInfo>,
    /// Each person's reaction on a message: (message id, person) → emoji.
    /// Bounded to `REACTIONS_KEPT` in memory and on disk at `reactions_path`.
    reactions: HashMap<(String, String), String>,
    reacted_order: VecDeque<(String, String)>,
    /// `{support}/signal/{aci}.reactions`. Empty when nothing is kept on disk.
    reactions_path: PathBuf,
    reactions_loaded: bool,
    typed: HashMap<String, Instant>,
    /// The one incoming call we ring for: its id, conversation, caller, kind, and when the
    /// offer was sent. The newest offer replaces an older one, so this stays one entry, and
    /// only the offer sitting here ends its own ring.
    ring: Option<Ring>,
    /// Active outgoing or joined Signal 1:1 call managed by this adapter.
    active_call: Option<ActiveSignalCall>,
    /// Active Signal group call we are currently joined in.
    active_group_call: Option<ActiveGroupCall>,
    /// Live group-call state per group conversation id (`g.<hex>`), tracked from
    /// `groupCallUpdate`, SFU peeks, and participant signals.
    group_calls: HashMap<String, GroupCallTrack>,
    /// Pending incoming Signal group-call ring (`conversation_id`, `group_id`, `era_id`, `caller_aci`, `ring_id`).
    group_ring: Option<IncomingGroupRing>,
    /// The user's own Signal settings. Off until the phone says.
    read_receipts: bool,
    typing: bool,
    /// File sends, by the time Signal gave them, so the phone's copy does not replace the file.
    echoed: VecDeque<u64>,
    /// Disappearing-messages timer per conversation id.
    timers: HashMap<String, wire::Timer>,
    /// Incoming unread messages that carry a disappearing-messages timer, keyed by (conversation, message_id).
    expiring: HashMap<(String, String), u32>,
    /// Conversations currently archived (filed on Shelf::Archive).
    archived: HashSet<String>,
    /// Ordered pinned conversation ids (lowest index sorts first).
    pinned: Vec<String>,
    /// Per-conversation mute expiry in ms since 1970 (0 = unmuted; >= i64::MAX = forever).
    muted_until: HashMap<String, u64>,
    /// Groups where mentions do not notify while the group is muted.
    dont_notify_mentions: HashSet<String>,
    /// Account setting: keep muted chats archived when new messages arrive.
    keep_muted_archived: bool,
    /// Chat folders synced from the phone's storage service.
    folders: Vec<wire::ChatFolder>,
    /// Conversations known to this adapter, for re-evaluating folder membership.
    known_conversations: HashSet<String>,
    /// ACIs in our contacts list.
    contacts: HashSet<String>,
    /// Blocked conversation ids (direct ACIs and `g.<hex>` group ids).
    blocked: HashSet<String>,
    /// Conversations whose message request has been accepted (or known contacts/groups).
    accepted: HashSet<String>,
    /// Direct conversations currently waiting as unaccepted message requests.
    requests: HashSet<String>,
    /// Recently searched GIF entries keyed by catalog item id, bounded to 256 entries.
    gifs: HashMap<String, wire::GifEntry>,
    /// Synced call history rows (`(conversation_id, call_id)`), bounded to 500 entries.
    call_history: VecDeque<(String, u64)>,
    /// Created or synced call links keyed by their 32-byte root key.
    call_links: HashMap<[u8; 32], wire::CallLink>,
    /// Pinned messages across conversations, persisted at `pins_path`.
    pinned_messages: Vec<PinRecord>,
    /// `{support}/signal/{aci}.pins`. Empty when nothing is kept on disk.
    pins_path: PathBuf,
    pins_loaded: bool,
    /// Received encrypted attachment pointers waiting to be downloaded on `Command::Wanted`.
    held_attachments: HashMap<String, HeldAttachment>,
    held_order: VecDeque<String>,
    /// `{support}/signal/{aci}.attachments`. Holds pointers across restarts and from history transfer.
    attachments_path: PathBuf,
    attachments_loaded: bool,
    /// Installed Signal sticker packs (persisted to `{support}/signal/{aci}.stickers`).
    sticker_packs: Vec<wire::StickerPackInfo>,
    stickers_path: PathBuf,
    stickers_loaded: bool,
    /// Sticker catalog preview tiles waiting to be downloaded on `Command::Wanted`.
    held_stickers: HashMap<String, HeldSticker>,
    /// True when Signal Stories are turned off on this account.
    stories_disabled: bool,
    /// True when story view receipts (`storyViewReceiptsEnabled`) are enabled.
    story_view_receipts: bool,
    /// Synced Signal story distribution lists (`StoryDistributionListRecord`).
    story_lists: Vec<wire::StoryDistributionList>,
    /// Active stories remembered for reply snippet attribution, permission checks, audience, and viewer tracking.
    stories: HashMap<String, HeldStory>,
}

/// Metadata for one active Signal story (expires 24 hours after `sent`).
#[derive(Clone)]
struct HeldStory {
    author: String,
    sent: u64,
    group_id: Option<String>,
    allows_replies: bool,
    snippet: String,
    expires_at: i64,
    /// Recipient ACIs for a direct story. A group story leaves this empty; its members are the audience.
    audience: Vec<String>,
    viewed_by: Vec<String>,
}

/// One sticker catalog preview tile remembered for lazy download/decrypt on `Command::Wanted`.
#[derive(Clone)]
struct HeldSticker {
    pack_id: String,
    pack_key: String,
    sticker_id: u32,
    file: MediaFile,
    address: String,
}

/// One received Signal attachment pointer remembered for lazy download/decrypt on `Command::Wanted`.
#[derive(Clone)]
struct HeldAttachment {
    pointer: wire::Pointer,
    file: MediaFile,
    address: String,
    conversation: String,
    message_id: String,
}

/// What [`Event::Call`] told of, for [`Event::CallEnded`] or [`Command::CallJoin`] to handle it.
#[derive(Clone)]
struct Ring {
    id: u64,
    conversation: String,
    author: String,
    video: bool,
    sent: u64,
    sdp: String,
    opaque: Vec<u8>,
    pending_ice: Vec<String>,
}

/// Active outgoing or joined Signal 1:1 call state.
#[derive(Clone)]
struct ActiveSignalCall {
    id: u64,
    conversation: String,
    peer_aci: String,
    video: bool,
    outgoing: bool,
    answered: bool,
    muted: bool,
    deafened: bool,
    camera: bool,
    sharing: bool,
    pending_ice: Vec<String>,
    sent_ms: u64,
}

#[derive(Clone, Default)]
struct GroupMemberCallState {
    muted: bool,
    camera: bool,
    sharing: bool,
    hand_raised: bool,
    reaction: Option<String>,
}

/// Tracked state of a live Signal group call for one group conversation (`g.<hex>`).
#[derive(Clone, Default)]
struct GroupCallTrack {
    era_id: String,
    members: Vec<String>,
    full: bool,
    states: HashMap<String, GroupMemberCallState>,
    /// Verified group members whose sender key we accepted for this era.
    sender_keys: HashMap<String, Vec<u8>>,
}

/// Incoming group-call ring waiting to be accepted, declined, or cancelled.
#[derive(Clone)]
struct IncomingGroupRing {
    conversation: String,
    group_id: String,
    era_id: String,
    caller: String,
    ring_id: i64,
}

/// Active Signal group call we have joined through the SFU.
#[derive(Clone)]
struct ActiveGroupCall {
    conversation: String,
    group_id: String,
    era_id: String,
    muted: bool,
    deafened: bool,
    camera: bool,
    sharing: bool,
    hand_raised: bool,
    reaction: Option<String>,
    ringing: Vec<CallRing>,
}

impl Live {
    fn name(&self, id: &str) -> String {
        if let Some(nick) = self.nicknames.get(id).filter(|s| !s.is_empty()) {
            return nick.clone();
        }
        self.names.get(id).cloned().unwrap_or_else(|| NO_NAME.to_owned())
    }

    fn known_name(&self, id: &str) -> String {
        if let Some(nick) = self.nicknames.get(id).filter(|s| !s.is_empty()) {
            return nick.clone();
        }
        self.names.get(id).cloned().unwrap_or_default()
    }

    fn unread(&mut self, conversation: &str, author: &str, sent: u64) {
        if !self.unread.contains_key(conversation) && self.unread.len() >= CONVERSATIONS_KEPT {
            let oldest = self.unread.iter().min_by_key(|(_, held)| held.back().map_or(0, |(_, sent)| *sent)).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.unread.remove(&oldest);
            }
        }
        let held = self.unread.entry(conversation.to_owned()).or_default();
        if held.len() >= UNREAD_KEPT {
            held.pop_front();
        }
        held.push_back((author.to_owned(), sent));
    }

    /// Takes the unread messages of a conversation up to and including `upto`.
    fn read_upto(&mut self, conversation: &str, upto: u64) -> Vec<(String, u64)> {
        let Some(held) = self.unread.get_mut(conversation) else {
            return Vec::new();
        };
        let read: Vec<_> = held.iter().filter(|(_, sent)| *sent <= upto).cloned().collect();
        held.retain(|(_, sent)| *sent > upto);
        if held.is_empty() {
            self.unread.remove(conversation);
        }
        read
    }

    fn sent(&mut self, sent: u64, conversation: &str) {
        if self.sent.len() >= SENT_KEPT {
            self.sent.pop_front();
        }
        self.sent.push_back((sent, conversation.to_owned()));
    }

    /// Loads the reaction record from disk once, keeping at most `REACTIONS_KEPT` newest rows.
    fn load_reactions(&mut self) {
        if self.reactions_loaded {
            return;
        }
        self.reactions_loaded = true;
        let rows = read_reactions(&self.reactions_path);
        let skip = rows.len().saturating_sub(REACTIONS_KEPT);
        for row in rows.into_iter().skip(skip) {
            let key = (row.message, row.person);
            if !self.reactions.contains_key(&key) {
                self.reacted_order.push_back(key.clone());
            }
            self.reactions.insert(key, row.emoji);
        }
    }

    /// This send carried a file. The phone's later copy of the same send is ignored.
    fn echoed(&mut self, sent: u64) {
        if self.echoed.len() >= SENT_KEPT {
            self.echoed.pop_front();
        }
        self.echoed.push_back(sent);
    }

    fn echoed_file(&self, sent: u64) -> bool {
        self.echoed.contains(&sent)
    }

    fn note_timer(&mut self, conversation: &str, seconds: u32, version: u32) {
        if seconds == 0 {
            self.timers.remove(conversation);
        } else {
            self.timers.insert(conversation.to_owned(), wire::Timer { seconds, version });
        }
    }

    fn timer_of(&self, conversation: &str) -> wire::Timer {
        if let Some(timer) = self.timers.get(conversation) {
            return *timer;
        }
        if self.default_timer > 0 && !conversation.starts_with("g.") {
            return wire::Timer { seconds: self.default_timer, version: 1 };
        }
        wire::Timer::default()
    }

    fn remember_expiring(&mut self, conversation: &str, message_id: &str, seconds: u32) {
        if seconds == 0 {
            return;
        }
        if self.expiring.len() >= REACTIONS_KEPT {
            self.expiring.clear();
        }
        self.expiring.insert((conversation.to_owned(), message_id.to_owned()), seconds);
    }

    fn take_expiring(&mut self, conversation: &str, message_id: &str) -> Option<u32> {
        self.expiring.remove(&(conversation.to_owned(), message_id.to_owned()))
    }

    fn is_muted(&self, conversation: &str) -> bool {
        self.muted_until.get(conversation).is_some_and(|&until| until > now_ms())
    }

    fn filing_for(&self, conversation: &str, unread: bool) -> Filing {
        let is_group = conversation.starts_with("g.");
        let muted = self.is_muted(conversation);
        let folders = self
            .folders
            .iter()
            .filter(|folder| folder_holds(folder, conversation, is_group, muted, unread))
            .map(|folder| folder.id.clone())
            .collect();
        Filing {
            folders,
            archived: self.archived.contains(conversation),
            pin: self.pinned.iter().position(|id| id == conversation).map(|at| at as i32),
            hidden: false,
            followed: false,
        }
    }

    fn folder_rows(&self) -> Vec<FolderRow> {
        self.folders
            .iter()
            .enumerate()
            .map(|(at, folder)| FolderRow {
                id: folder.id.clone(),
                name: folder.name.clone(),
                position: at as i32,
                color: None,
                spaces: Vec::new(),
                pinned: Vec::new(),
            })
            .collect()
    }

    /// Records a person's reaction, answering the one it replaced. Both memory and the file on
    /// disk are capped at `REACTIONS_KEPT`, oldest evicted first.
    fn react(&mut self, message: &str, by: &str, emoji: Option<&str>) -> Option<String> {
        self.load_reactions();
        let key = (message.to_owned(), by.to_owned());
        let mut changed = false;
        let before = match emoji {
            Some(emoji) => {
                if self.reactions.get(&key).map(String::as_str) == Some(emoji) {
                    Some(emoji.to_owned())
                } else {
                    changed = true;
                    if let Some(pos) = self.reacted_order.iter().position(|k| k == &key) {
                        self.reacted_order.remove(pos);
                    } else {
                        while self.reacted_order.len() >= REACTIONS_KEPT {
                            if let Some(old) = self.reacted_order.pop_front() {
                                self.reactions.remove(&old);
                            }
                        }
                    }
                    self.reacted_order.push_back(key.clone());
                    self.reactions.insert(key, emoji.to_owned())
                }
            }
            None => {
                let removed = self.reactions.remove(&key);
                if removed.is_some() {
                    changed = true;
                    self.reacted_order.retain(|k| k != &key);
                }
                removed
            }
        };
        if changed {
            write_reactions(&self.reactions_path, &self.reacted_order, &self.reactions);
        }
        before.filter(|before| Some(before.as_str()) != emoji)
    }

    fn load_pins(&mut self) {
        if self.pins_loaded {
            return;
        }
        self.pins_loaded = true;
        self.pinned_messages = read_pins(&self.pins_path);
    }

    fn record_pin(&mut self, conversation: &str, message_id: &str, pinned_by: String, pinned_at: i64, expires_at: Option<i64>) {
        self.load_pins();
        self.pinned_messages
            .retain(|p| !(p.conversation == conversation && p.message_id == message_id));
        self.pinned_messages.push(PinRecord {
            conversation: conversation.to_owned(),
            message_id: message_id.to_owned(),
            pinned_by,
            pinned_at,
            expires_at,
        });
        if self.pinned_messages.len() > 512 {
            let extra = self.pinned_messages.len() - 512;
            self.pinned_messages.drain(..extra);
        }
        write_pins(&self.pins_path, &self.pinned_messages);
    }

    fn remove_pin(&mut self, conversation: &str, message_id: &str) -> bool {
        self.load_pins();
        let before = self.pinned_messages.len();
        self.pinned_messages
            .retain(|p| !(p.conversation == conversation && p.message_id == message_id));
        let changed = self.pinned_messages.len() != before;
        if changed {
            write_pins(&self.pins_path, &self.pinned_messages);
        }
        changed
    }

    fn prune_expired_pins(&mut self, now: i64) -> Vec<(String, String)> {
        self.load_pins();
        let mut expired = Vec::new();
        self.pinned_messages.retain(|p| {
            if p.expires_at.is_some_and(|exp| exp <= now) {
                expired.push((p.conversation.clone(), p.message_id.clone()));
                false
            } else {
                true
            }
        });
        if !expired.is_empty() {
            write_pins(&self.pins_path, &self.pinned_messages);
        }
        expired
    }

    fn load_attachments(&mut self, force: bool) {
        if self.attachments_loaded && !force {
            return;
        }
        self.attachments_loaded = true;
        for row in transfer::read_kept_attachments(&self.attachments_path) {
            if !self.held_attachments.contains_key(&row.id) {
                while self.held_order.len() >= REACTIONS_KEPT {
                    if let Some(old) = self.held_order.pop_front() {
                        self.held_attachments.remove(&old);
                    }
                }
                self.held_order.push_back(row.id.clone());
                self.held_attachments.insert(
                    row.id,
                    HeldAttachment {
                        pointer: row.pointer,
                        file: row.file,
                        address: row.address,
                        conversation: row.conversation,
                        message_id: row.message_id,
                    },
                );
            }
        }
    }

    fn remember_attachment(&mut self, id: String, held: HeldAttachment) {
        self.load_attachments(false);
        if !self.held_attachments.contains_key(&id) {
            while self.held_order.len() >= REACTIONS_KEPT {
                if let Some(old) = self.held_order.pop_front() {
                    self.held_attachments.remove(&old);
                }
            }
            self.held_order.push_back(id.clone());
        }
        self.held_attachments.insert(id, held);
        let rows: Vec<transfer::KeptAttachment> = self
            .held_order
            .iter()
            .filter_map(|k| {
                let h = self.held_attachments.get(k)?;
                Some(transfer::KeptAttachment {
                    id: k.clone(),
                    pointer: h.pointer.clone(),
                    file: h.file.clone(),
                    address: h.address.clone(),
                    conversation: h.conversation.clone(),
                    message_id: h.message_id.clone(),
                })
            })
            .collect();
        transfer::write_kept_attachments(&self.attachments_path, &rows);
    }

    fn pins_for(&mut self, conversation: &str) -> Vec<PinRecord> {
        self.load_pins();
        let mut out: Vec<PinRecord> = self
            .pinned_messages
            .iter()
            .filter(|p| p.conversation == conversation)
            .cloned()
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.pinned_at));
        out.truncate(protocol::PINNED_LIMIT);
        out
    }
}

/// One pinned message remembered for a Signal conversation (Signal has no server pin index).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinRecord {
    conversation: String,
    message_id: String,
    pinned_by: String,
    pinned_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
}

fn pins_path(aci: &str) -> PathBuf {
    protocol::support_dir().join("signal").join(format!("{aci}.pins"))
}

fn read_pins(path: &Path) -> Vec<PinRecord> {
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > REACTIONS_FILE_LIMIT {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<PinRecord>>(&bytes).unwrap_or_default()
}

fn write_pins(path: &Path, rows: &[PinRecord]) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(bytes) = serde_json::to_vec(rows) else {
        return;
    };
    let tmp = path.with_extension("pins.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// One kept reaction. The file is a list of these in oldest-to-newest order, owned by this
/// adapter, not the message store.
#[derive(Serialize, Deserialize)]
struct KeptReaction {
    message: String,
    person: String,
    emoji: String,
}

fn reactions_path(aci: &str) -> PathBuf {
    protocol::support_dir().join("signal").join(format!("{aci}.reactions"))
}

fn stickers_path(aci: &str) -> PathBuf {
    protocol::support_dir().join("signal").join(format!("{aci}.stickers"))
}

fn read_sticker_packs(path: &Path) -> Vec<wire::StickerPackInfo> {
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<wire::StickerPackInfo>>(&bytes).unwrap_or_default()
}

fn write_sticker_packs(path: &Path, packs: &[wire::StickerPackInfo]) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(packs) {
        let temp = path.with_extension("tmp");
        if std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path)).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
    }
}

fn read_reactions(path: &Path) -> Vec<KeptReaction> {
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > REACTIONS_FILE_LIMIT {
        tracing::warn!("chatd: Signal: skipping oversized reaction record ({} bytes)", meta.len());
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<KeptReaction>>(&bytes).unwrap_or_default()
}

fn write_reactions(path: &Path, order: &VecDeque<(String, String)>, book: &HashMap<(String, String), String>) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            tracing::warn!("chatd: Signal: the reaction record directory could not be created");
            return;
        }
    }
    let rows: Vec<KeptReaction> = order
        .iter()
        .filter_map(|key| {
            let emoji = book.get(key)?;
            Some(KeptReaction { message: key.0.clone(), person: key.1.clone(), emoji: emoji.clone() })
        })
        .collect();
    let Ok(bytes) = serde_json::to_vec(&rows) else {
        return;
    };
    let temp = path.with_extension("tmp");
    if std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path)).is_err() {
        let _ = std::fs::remove_file(&temp);
        tracing::warn!("chatd: Signal: the reaction record could not be kept");
    }
}

/// The lab's signal-wave13 driver: proves mobile companion linking (#735) and linked-device
/// message history transfer (#724) end-to-end against a scratch SQLite store.
pub fn prove_signal_wave13() -> Result<(), String> {
    use protocol::{AccountKey, Network, Page};
    use std::sync::atomic::AtomicBool;

    if std::env::var_os("TINYCLIENT_DIR").is_none() {
        return Err("a scratch directory is required".into());
    }
    // 1. Verify #735 / #724 link URI advertises companion transfer capability.
    if !fake::LINK.starts_with("sgnl://linkdevice?") || !fake::LINK.contains("capabilities=transfer") {
        return Err(format!("Signal link URI must advertise transfer capability: {}", fake::LINK));
    }

    // 2. Verify #724 streaming encryption, HMAC verification, gzip decompression, protobuf decode,
    //    disappearing-timer enforcement, attachment pointer persistence, and idempotent import.
    let dir = protocol::support_dir().join("prove-wave13");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let store = crate::store::Store::open(&dir.join("store.db")).map_err(|err| err.to_string())?;
    let hub = Arc::new(crate::hub::Hub::new(store));
    let me = fake::ME;
    let peer = fake::GRACE;
    let account = AccountKey { network: Network::Signal, account: me.into() };
    let key = [0x5cu8; 32];
    let mk = [0x19u8; 32];
    let now_ms = (crate::hub::now() as u64) * 1_000;

    let (archive_bytes, gid) =
        transfer::build_fixture_archive(&key, me, peer, "Grace Ombra", &mk, "Bridge club", now_ms, 10)?;
    let archive_path = dir.join("transfer.bin");
    std::fs::write(&archive_path, &archive_bytes).map_err(|err| err.to_string())?;

    let skip = AtomicBool::new(false);
    let first = transfer::import_archive(&hub, &account, me, "Ada Quill", &key, &archive_path, &|_, _| {}, &skip)?;
    if first.chats != 2 || first.messages != 13 || first.new_messages != 13 || first.skipped_expired != 1 {
        return Err(format!("unexpected first import summary: {first:?}"));
    }

    // Re-importing the exact same archive must be idempotent (0 new messages, no duplicates).
    let second = transfer::import_archive(&hub, &account, me, "Ada Quill", &key, &archive_path, &|_, _| {}, &skip)?;
    if second.new_messages != 0 {
        return Err(format!("re-import duplicated messages: {second:?}"));
    }

    let dm_key = protocol::Key { network: account.network, account: account.account.clone(), id: peer.to_owned() };
    let dm = hub
        .store()
        .messages(&dm_key, &Page::Newest, 20, crate::hub::now())
        .map_err(|e| e.to_string())?
        .map(|p| p.messages)
        .unwrap_or_default();
    if dm.len() != 2 {
        return Err(format!("expected 2 non-expired direct messages, got {}", dm.len()));
    }
    let grp_key = protocol::Key {
        network: account.network,
        account: account.account.clone(),
        id: Peer::Group(gid).conversation(),
    };
    let grp = hub
        .store()
        .messages(&grp_key, &Page::Newest, 20, crate::hub::now())
        .map_err(|e| e.to_string())?
        .map(|p| p.messages)
        .unwrap_or_default();
    if grp.len() != 11 {
        return Err(format!("expected 11 group messages, got {}", grp.len()));
    }

    // Tampered archive must be rejected by HMAC verification.
    let mut bad = archive_bytes;
    bad[24] ^= 0x01;
    let bad_path = dir.join("tampered.bin");
    std::fs::write(&bad_path, &bad).map_err(|err| err.to_string())?;
    if transfer::import_archive(&hub, &account, me, "Ada Quill", &key, &bad_path, &|_, _| {}, &skip).is_ok() {
        return Err("tampered archive was accepted".into());
    }
    Ok(())
}

/// The lab's signal-reaction-restart driver. A build without this keeps running.
pub fn prove_signal_reaction_restart() -> Result<(), String> {
    if std::env::var_os("TINYCLIENT_DIR").is_none() {
        return Err("a scratch directory is required".into());
    }
    let path = protocol::support_dir().join("signal").join("prove-reaction-restart.reactions");
    let _ = std::fs::remove_file(&path);
    let message = "1791892801000-b2b2b2b2-0000-4000-8000-000000000002";
    let person = "b2b2b2b2-0000-4000-8000-000000000002";
    let mut live = Live { reactions_path: path.clone(), ..Live::default() };
    if live.react(message, person, Some("👍")).is_some() {
        return Err("a first reaction replaced one that was not there".into());
    }
    // What a restart is: the memory is new, the file is the same.
    let mut live = Live { reactions_path: path, ..Live::default() };
    match live.react(message, person, Some("❤️")) {
        Some(old) if old == "👍" => {}
        other => return Err(format!("a restart forgot the reaction, got {other:?}")),
    }
    let held = read_reactions(&live.reactions_path)
        .into_iter()
        .find(|row| row.message == message && row.person == person)
        .map(|row| row.emoji);
    if held.as_deref() != Some("❤️") {
        return Err(format!("the kept reaction is {held:?}"));
    }
    Ok(())
}

/// One connection's worth of an account: what both threads of the adapter share.
struct Account {
    ctx: Context,
    wire: Box<dyn Wire>,
    me: String,
    name: String,
    live: Arc<Mutex<Live>>,
    /// Conversations whose picture has been asked of Signal on this connection.
    pictured: Mutex<HashSet<String>>,
}

impl Account {
    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.aci != ctx.account.account {
        return Err("the Signal session is for another account".to_owned());
    }
    if !supported() {
        return Err(unsupported_reason());
    }
    // What is learned (names, settings, unread) outlives one connection. Reactions outlive the
    // process: the file is what a later reaction replaces, once this memory is gone.
    let live = Arc::new(Mutex::new(Live {
        reactions_path: reactions_path(&saved.aci),
        pins_path: pins_path(&saved.aci),
        stickers_path: stickers_path(&saved.aci),
        attachments_path: transfer::attachments_path(&saved.aci),
        ..Live::default()
    }));
    let current: Arc<Mutex<Option<Arc<Account>>>> = Arc::default();
    let (socket_ctx, held, socket_saved, socket_live) = (ctx.clone(), current.clone(), saved.clone(), live.clone());
    std::thread::Builder::new()
        .name("signal-events".into())
        .spawn(move || {
            keep_connected(&socket_ctx, || connect(&socket_ctx, &socket_saved, &socket_live, &held));
            held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        })
        .map_err(|err| err.to_string())?;
    std::thread::Builder::new()
        .name("signal-commands".into())
        .spawn(move || {
            let account = || current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            serve_commands(
                &ctx,
                inbox,
                PACE,
                // Signal keeps no history on its servers: what is stored is all there is.
                |_| {},
                |command| match (account(), command) {
                    (Some(account), command) => serve(&account, command),
                    (None, Command::SignOut(reply)) => {
                        forget(&saved);
                        let _ = reply.send(Ok(()));
                    }
                    (None, command) => offline(&ctx, command),
                },
            );
            // The adapter was dropped (a sign-out, a new sign-in): the connection goes with it,
            // which is also what lets the thread that follows its events end.
            if let Some(account) = account() {
                account.wire.close();
            }
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// One connection, from opening the wire to losing it.
fn connect(ctx: &Context, saved: &Saved, live: &Arc<Mutex<Live>>, current: &Mutex<Option<Arc<Account>>>) -> Failure {
    let wire = match open(saved) {
        Ok(wire) => wire,
        Err(why) => return Failure::Dropped(why),
    };
    let Some(events) = wire.events() else {
        return Failure::Dropped("the wire has no events".to_owned());
    };
    let (me, name) = wire.me();
    let name = if name.is_empty() { saved.name.clone() } else { name };
    let account = Arc::new(Account { ctx: ctx.clone(), wire, me, name, live: live.clone(), pictured: Mutex::default() });
    let _ = ctx.describe(if account.name.is_empty() { "Signal" } else { &account.name }, &account.me);
    show_self(&account);
    // Nothing is counted until an adapter says so; Signal counts its own body limit.
    ctx.capabilities(capabilities(account.live().link_previews));
    *current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(account.clone());
    // Dropped while connecting: the command thread has gone and will not close this wire.
    if ctx.stopped() {
        account.wire.close();
        return Failure::Dropped("stopped".to_owned());
    }
    // Older builds may have cached our own conversation under the contact fallback.
    // Repair it on reconnect without creating an empty self conversation.
    if matches!(ctx.known(&account.me), Ok(Some(_))) {
        ensure_row(&account, &Peer::Direct(account.me.clone()), "");
    }
    ctx.link(Link::Connected);
    let ended = loop {
        match events.recv() {
            Ok(Event::Closed(why)) => break Failure::Dropped(why),
            Ok(Event::Ended(why)) => break Failure::SignedOut(why),
            Ok(event) => apply(&account, event),
            Err(_) => break Failure::Dropped("the connection closed".to_owned()),
        }
        if ctx.stopped() {
            break Failure::Dropped("stopped".to_owned());
        }
    };
    current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    account.wire.close();
    ended
}

/// Evaluates a Signal chat folder's filter rules against one conversation.
fn folder_holds(folder: &wire::ChatFolder, conversation: &str, is_group: bool, muted: bool, unread: bool) -> bool {
    if folder.excluded.iter().any(|p| p.conversation() == conversation) {
        return false;
    }
    if folder.show_only_unread && !unread {
        return false;
    }
    if !folder.show_muted && muted {
        return false;
    }
    if folder.included.iter().any(|p| p.conversation() == conversation) {
        return true;
    }
    if !is_group && folder.include_all_individual {
        return true;
    }
    if is_group && folder.include_all_groups {
        return true;
    }
    false
}

/// Recomputes and writes a conversation's filing (folders, archive, pin) based on its current unread & mute state.
fn sync_filing(account: &Account, conversation: &str) {
    let unread = account.ctx.known(conversation).ok().flatten().is_some_and(|row| row.unread > 0);
    let filing = account.live().filing_for(conversation, unread);
    let _ = account.ctx.filing(conversation, &filing);
}

/// The row of a conversation, recorded the first time it is seen.
fn ensure_row(account: &Account, peer: &Peer, title: &str) {
    // The phone need not include ourselves in its contact list.
    let title = if matches!(peer, Peer::Direct(aci) if aci == &account.me) { "Note to Self" } else { title };
    let id = peer.conversation();
    let is_request = {
        let mut live = account.live();
        live.known_conversations.insert(id.clone());
        live.requests.contains(&id)
    };
    let special = if is_request { Special::Request } else { Special::None };
    match account.ctx.known(&id) {
        Ok(Some(known)) if (title.is_empty() || known.title == title) && known.special == special => {}
        // A picture fetched earlier stays the row's.
        Ok(Some(_)) | Ok(None) => {
            let mut next = row(peer, title);
            next.avatar = account.ctx.avatar_of(&id);
            next.special = special;
            drop(account.ctx.conversation(&next));
        }
        Err(err) => tracing::warn!("chatd: Signal: {err}"),
    }
}

fn row(peer: &Peer, title: &str) -> ConversationRow {
    match peer {
        Peer::Direct(aci) => {
            let title = if title.is_empty() { NO_NAME } else { title };
            let mut row = ConversationRow::new(aci.clone(), ConversationKind::Direct, title);
            row.peer = Some(aci.clone());
            row.listed = true;
            row.private = true;
            row.rights.pin = true;
            row.call = call_offer(ConversationKind::Direct).support();
            row
        }
        Peer::Group(_) => {
            let title = if title.is_empty() { "Signal group" } else { title };
            let mut row = ConversationRow::new(peer.conversation(), ConversationKind::Group, title);
            row.listed = true;
            row.private = true;
            row.call = call_offer(ConversationKind::Group).support();
            row
        }
    }
}

/// A message as the contract has it.
fn message(account: &Account, conversation: &str, text: &Text, outgoing: bool) -> Message {
    let author = if outgoing { account.me.as_str() } else { text.author.as_str() };
    let name = if outgoing {
        account.name.clone()
    } else if text.name.is_empty() {
        account.live().name(author)
    } else {
        text.name.clone()
    };
    let (words, shift) = match (&text.unsupported, text.text.is_empty()) {
        (Some(what), true) => (what.clone(), None),
        (Some(what), false) => (format!("{what}: {}", text.text), Some(format!("{what}: ").len() as u32)),
        (None, _) => (text.text.clone(), Some(0)),
    };
    let sent = i64::try_from(text.sent / 1_000).unwrap_or(0);
    let msg_id = wire::message_id(text.sent, author);
    let mut message = Message::plain(msg_id.clone(), name, words, sent, outgoing);
    message.sender.id = author.to_owned();
    message.mentions_me = text.mentions_me && !outgoing;
    if let Some(shift) = shift {
        message.spans = text.spans.iter().map(|span| Span { start: span.start + shift, end: span.end + shift, style: span.style.clone() }).collect();
    }
    if text.unsupported.is_some() {
        message.kind = MessageKind::Unsupported { link: String::new() };
    }
    if !text.attachments.is_empty() {
        let mut to_register = Vec::with_capacity(text.attachments.len());
        for (idx, ptr) in text.attachments.iter().enumerate() {
            let media_id = format!("signal:att:{}:{}:{idx}", text.sent, author);
            let address = format!("signal://attachment/{media_id}");
            let kind = files::attachment_kind(&ptr.content_type, ptr.flags);
            let mut media = MediaFile::new(media_id.clone(), kind);
            media.width = ptr.width;
            media.height = ptr.height;
            media.bytes = u64::from(ptr.size);
            to_register.push((media.clone(), address.clone()));
            account.live().remember_attachment(
                media_id.clone(),
                HeldAttachment {
                    pointer: ptr.clone(),
                    file: media.clone(),
                    address,
                    conversation: conversation.to_owned(),
                    message_id: msg_id.clone(),
                },
            );
            let preview_bytes = if !ptr.thumbnail.is_empty() && crate::blobs::is_picture(&ptr.thumbnail) {
                Some(ptr.thumbnail.clone())
            } else if let Some(hash) = ptr.blurhash.as_deref() {
                files::decode_blurhash_jpeg(hash, ptr.width, ptr.height)
            } else {
                None
            };
            let preview = preview_bytes.and_then(|bytes| {
                let mut prev = MediaFile::new(format!("{media_id}:preview"), MediaKind::Image);
                prev.bytes = bytes.len() as u64;
                account
                    .ctx
                    .landed(&prev, &format!("signal://preview/{media_id}"), &bytes)
            });
            let att_name = if !ptr.file_name.trim().is_empty() {
                ptr.file_name.trim().to_owned()
            } else {
                files::default_filename(&ptr.content_type, ptr.flags).to_owned()
            };
            if message.text.is_empty() && !ptr.caption.is_empty() {
                message.text = ptr.caption.clone();
            }
            let mut attachment = Attachment::new(att_name, media);
            attachment.mime = ptr.content_type.clone();
            if kind == MediaKind::Video {
                attachment.thumb = preview.clone();
            }
            if ptr.is_voice() {
                attachment.duration = 1;
                attachment.waveform = vec![24; 32];
            }
            attachment.preview = preview;
            message.attachments.push(attachment);
        }
        if !to_register.is_empty() {
            let _ = account.ctx.media(&to_register);
        }
    }
    if let Some(st) = &text.sticker {
        let media_id = format!("signal:att:{}:{}:sticker", text.sent, author);
        let address = format!("signal://attachment/{media_id}");
        let mut media = MediaFile::new(media_id.clone(), MediaKind::Image);
        media.width = if st.data.width > 0 { st.data.width } else { 512 };
        media.height = if st.data.height > 0 { st.data.height } else { 512 };
        media.bytes = u64::from(st.data.size);
        let _ = account.ctx.media(&[(media.clone(), address.clone())]);
        account.live().remember_attachment(
            media_id,
            HeldAttachment {
                pointer: st.data.clone(),
                file: media.clone(),
                address,
                conversation: conversation.to_owned(),
                message_id: msg_id.clone(),
            },
        );
        let name = if !st.emoji.is_empty() { st.emoji.clone() } else { "sticker.webp".to_owned() };
        let mut attachment = Attachment::new(name, media);
        attachment.mime = if st.data.content_type.is_empty() {
            "image/webp".to_owned()
        } else {
            st.data.content_type.clone()
        };
        attachment.sticker = true;
        message.attachments.push(attachment);
    }
    message.contacts = text.contacts.clone();
    for (idx, preview) in text.previews.iter().enumerate() {
        if !wire::preview_url_in_body(&text.text, &preview.url) {
            continue;
        }
        let image = preview.image.as_ref().filter(|b| !b.is_empty()).and_then(|bytes| {
            let mut file = MediaFile::new(format!("signal:preview:{}:{}:{idx}", text.sent, author), MediaKind::Image);
            file.width = preview.image_width;
            file.height = preview.image_height;
            file.bytes = bytes.len() as u64;
            account
                .ctx
                .landed(&file, &format!("signal://preview/{}/{}/{idx}", text.sent, author), bytes)
        });
        message.previews.push(protocol::Preview {
            url: preview.url.clone(),
            site: crate::adapters::tiny::preview::real_host(&preview.url),
            title: preview.title.clone(),
            description: preview.description.clone(),
            image,
            duration: 0,
            opens_in_browser: false,
        });
    }
    if wire::find_group_invite_link(&message.text).is_some() {
        message.card = Some(Box::new(protocol::Card {
            title: "Signal group invite".to_owned(),
            text: "Joining by link shares your profile key with the group.".to_owned(),
            footer: "Join group".to_owned(),
            ..protocol::Card::default()
        }));
    } else if let Some((_, root_key)) = wire::find_call_link(&message.text) {
        let known = account.live().call_links.get(&root_key).cloned();
        let title = known
            .as_ref()
            .filter(|l| !l.name.is_empty())
            .map(|l| l.name.clone())
            .unwrap_or_else(|| "Signal call link".to_owned());
        let text = if known.as_ref().is_some_and(|l| l.approve_all) {
            "Admin approval required to join.".to_owned()
        } else {
            "Anyone with this link can join the call.".to_owned()
        };
        message.card = Some(Box::new(protocol::Card {
            title,
            text,
            footer: "Join call".to_owned(),
            ..protocol::Card::default()
        }));
    } else if wire::find_sticker_pack_url(&message.text).is_some() {
        message.card = Some(Box::new(protocol::Card {
            title: "Signal sticker pack".to_owned(),
            text: "Install this encrypted sticker pack on Signal.".to_owned(),
            footer: "Install sticker pack".to_owned(),
            ..protocol::Card::default()
        }));
    }
    message.reply_to = text
        .quote
        .as_ref()
        .map(|quote| reply_to(account, conversation, &msg_id, quote))
        .or_else(|| {
            let ctx = text.story_context.as_ref()?;
            let story_id = wire::message_id(ctx.sent, &ctx.author);
            let sender = if ctx.author == account.me {
                account.name.clone()
            } else {
                account.live().name(&ctx.author)
            };
            let snippet = account
                .live()
                .stories
                .get(&story_id)
                .map(|s| s.snippet.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Story".to_owned());
            Some(ReplyTo {
                id: story_id,
                sender,
                snippet: snippet.chars().take(200).collect(),
                person: Some(account.ctx.key(&ctx.author)),
                deleted: false,
                thumb: None,
                story: true,
            })
        });
    message
}

fn reply_to(account: &Account, conversation: &str, msg_id: &str, quote: &Quote) -> ReplyTo {
    let sender = if quote.author == account.me { account.name.clone() } else { account.live().name(&quote.author) };
    let mut snippet = crate::store::without_spoilers(&quote.text, &quote.spans);
    let mut thumb = None;
    if let Some(first) = quote.attachments.first() {
        if snippet.trim().is_empty() {
            snippet = files::attachment_label(&first.content_type, &first.file_name, 0);
        }
        if let Some(ptr) = &first.thumbnail {
            let thumb_id = format!("signal:quote:{}:{}:{msg_id}", quote.sent, quote.author);
            let address = format!("signal://attachment/{thumb_id}");
            let mut mf = MediaFile::new(thumb_id.clone(), MediaKind::Image);
            mf.width = ptr.width;
            mf.height = ptr.height;
            mf.bytes = u64::from(ptr.size);
            if !ptr.thumbnail.is_empty() && crate::blobs::is_picture(&ptr.thumbnail) {
                thumb = account.ctx.landed(&mf, &address, &ptr.thumbnail);
            } else {
                let _ = account.ctx.media(&[(mf.clone(), address.clone())]);
                account.live().remember_attachment(
                    thumb_id,
                    HeldAttachment {
                        pointer: ptr.clone(),
                        file: mf.clone(),
                        address,
                        conversation: conversation.to_owned(),
                        message_id: msg_id.to_owned(),
                    },
                );
                thumb = Some(mf);
            }
        }
    }
    ReplyTo {
        id: wire::message_id(quote.sent, &quote.author),
        sender,
        snippet: snippet.chars().take(200).collect(),
        person: Some(account.ctx.key(&quote.author)),
        deleted: false,
        thumb,
        story: false,
    }
}

fn apply(account: &Account, event: Event) {
    let ctx = &account.ctx;
    match event {
        Event::Contacts(contacts) => {
            {
                let mut live = account.live();
                for contact in &contacts {
                    live.contacts.insert(contact.id.clone());
                    live.accepted.insert(contact.id.clone());
                    live.requests.remove(&contact.id);
                    if !contact.name.is_empty() {
                        live.names.insert(contact.id.clone(), contact.name.clone());
                    }
                    if !contact.phone.is_empty() {
                        live.phones.insert(contact.id.clone(), contact.phone.clone());
                    }
                }
            }
            let people: Vec<PersonRow> = {
                let live = account.live();
                contacts
                    .iter()
                    .map(|contact| {
                        let display = live.known_name(&contact.id);
                        let handle = live.usernames.get(&contact.id).cloned().unwrap_or_default();
                        PersonRow {
                            id: contact.id.clone(),
                            name: if display.is_empty() { NO_NAME.to_owned() } else { display },
                            handle,
                            avatar: ctx.avatar_of(&contact.id),
                            bot: false,
                            status: String::new(),
                            link: String::new(),
                        }
                    })
                    .collect()
            };
            let _ = ctx.people(&people);
            let owned: Vec<(String, Relation)> = {
                let live = account.live();
                contacts
                    .iter()
                    .map(|contact| {
                        let rel = if live.blocked.contains(&contact.id) { Relation::Blocked } else { Relation::Contact };
                        (contact.id.clone(), rel)
                    })
                    .collect()
            };
            let _ = ctx.relations(&owned);
            // A row named before the contact was known takes the contact's name (preferring nickname).
            for contact in &contacts {
                let display = account.live().known_name(&contact.id);
                if !display.is_empty() && matches!(ctx.known(&contact.id), Ok(Some(_))) {
                    ensure_row(account, &Peer::Direct(contact.id.clone()), &display);
                }
            }
        }
        Event::Groups(groups) => {
            for group in groups {
                let peer = Peer::Group(group.id.clone());
                let conv = peer.conversation();
                let rights = group_rights(&group, &account.me);
                let is_admin = group.admins.iter().any(|id| id == &account.me);
                {
                    let mut live = account.live();
                    live.groups.insert(conv.clone(), group.member_ids.clone());
                    live.group_admins.insert(conv.clone(), group.admins.clone());
                    live.group_info.insert(conv.clone(), group.clone());
                    live.known_conversations.insert(conv.clone());
                    live.accepted.insert(conv.clone());
                    if group.timer_secs > 0 {
                        live.note_timer(&conv, group.timer_secs, 0);
                    }
                }
                let mut row = row(&peer, &group.title);
                row.topic = group.description.chars().take(300).collect();
                row.read_only = !group.member || (group.announcements_only && !is_admin);
                if group.member && group.announcements_only && !is_admin {
                    row.read_only_reason = "Only admins can send messages".to_owned();
                }
                row.rights = rights;
                row.avatar = ctx.avatar_of(&row.id);
                let _ = ctx.conversation(&row);
                if !account.live().folders.is_empty() {
                    sync_filing(account, &conv);
                }
            }
        }
        Event::GroupChanged { peer, changes, title, description, left, rejoined, .. } => {
            let id = peer.conversation();
            let known = ctx.known(&id).ok().flatten();
            if title.is_some() || description.is_some() || left || rejoined {
                let title = title.unwrap_or_else(|| known.as_ref().map(|row| row.title.clone()).unwrap_or_default());
                let mut row = row(&peer, &title);
                row.topic = description.unwrap_or_else(|| known.as_ref().map(|row| row.topic.clone()).unwrap_or_default()).chars().take(300).collect();
                // A later rename does not put a removed member back, while re-adding clears read-only.
                row.read_only = if left {
                    true
                } else if rejoined {
                    false
                } else {
                    known.as_ref().is_some_and(|row| row.read_only)
                };
                if let Some(known) = &known {
                    row.rights = known.rights;
                    if !rejoined {
                        row.read_only_reason = known.read_only_reason.clone();
                    }
                }
                row.avatar = ctx.avatar_of(&row.id);
                let _ = ctx.conversation(&row);
            }
            let sent_ms = now_ms();
            for (at, line) in changes.iter().enumerate() {
                let mut message = Message::plain(format!("signal-system-{sent_ms}-{at}"), String::new(), line.clone(), (sent_ms / 1_000) as i64, false);
                message.kind = MessageKind::System;
                let _ = ctx.message(&id, &message);
            }
        }
        Event::Message { peer, text, outgoing } => {
            let conversation = peer.conversation();
            // Drop messages from blocked senders or blocked groups.
            {
                let mut live = account.live();
                if live.blocked.contains(&conversation) || (!outgoing && live.blocked.contains(&text.author)) {
                    return;
                }
                if outgoing {
                    live.accepted.insert(conversation.clone());
                    live.requests.remove(&conversation);
                } else if let Peer::Direct(aci) = &peer {
                    if aci != &account.me && !live.contacts.contains(aci) && !live.accepted.contains(aci) {
                        live.requests.insert(conversation.clone());
                    }
                }
            }
            if !outgoing && !text.name.is_empty() {
                let mut live = account.live();
                if !live.names.contains_key(&text.author) {
                    live.names.insert(text.author.clone(), text.name.clone());
                }
            }
            match &peer {
                // A person's row takes the name the user knows them by (preferring nickname), once there is one.
                Peer::Direct(aci) => {
                    let title = account.live().known_name(aci);
                    ensure_row(account, &peer, &title);
                }
                Peer::Group(_) => {
                    if let Ok(None) = ctx.known(&conversation) {
                        let _ = ctx.conversation(&row(&peer, ""));
                    }
                }
            }
            // A file we sent is already stored with that file. The phone's copy of the same
            // send is text, and would replace the row.
            let had_file = outgoing
                && ctx
                    .stored_message(&conversation, &wire::message_id(text.sent, &account.me))
                    .ok()
                    .flatten()
                    .is_some_and(|stored| !stored.attachments.is_empty());
            if outgoing && (account.live().echoed_file(text.sent) || files::replace_outgoing_sync(had_file)) {
                return;
            }
            let mut message = message(account, &conversation, &text, outgoing);
            let mut needs_filing = false;
            if !outgoing {
                let (suppress_mention, unarchived) = {
                    let mut live = account.live();
                    let muted = live.is_muted(&conversation);
                    let suppress = muted && live.dont_notify_mentions.contains(&conversation);
                    let unarchived = if live.archived.contains(&conversation) && !(live.keep_muted_archived && muted) {
                        live.archived.remove(&conversation);
                        true
                    } else {
                        false
                    };
                    (suppress, unarchived)
                };
                if suppress_mention {
                    message.mentions_me = false;
                }
                needs_filing = unarchived || !account.live().folders.is_empty();
            }
            if text.expire_timer > 0 || text.expire_timer_version > 0 {
                account.live().note_timer(&conversation, text.expire_timer, text.expire_timer_version);
                let _ = ctx.disappearing(&conversation, (text.expire_timer > 0).then_some(text.expire_timer));
            }
            if text.expire_timer > 0 {
                if outgoing {
                    message.expires_at = Some(crate::hub::now() + i64::from(text.expire_timer));
                } else {
                    account.live().remember_expiring(&conversation, &message.id, text.expire_timer);
                }
            }
            if outgoing {
                account.live().sent(text.sent, &conversation);
            } else {
                account.live().unread(&conversation, &text.author, text.sent);
            }
            let _ = ctx.message(&conversation, &message);
            if needs_filing {
                sync_filing(account, &conversation);
            }
        }
        Event::Edited { peer, author, target, at, text, spans } => {
            // A late edit is ignored by every other client. Keeping it here would show words
            // nobody else has.
            if !accept_remote(target, now_ms(), note_to_self(account, &peer)) {
                return;
            }
            let conversation = peer.conversation();
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &wire::message_id(target, &author)) {
                message.text = text;
                message.spans = spans;
                message.edited = true;
                message.edited_at = (at > 0).then(|| i64::try_from(at / 1_000).unwrap_or(0));
                let _ = ctx.message(&conversation, &message);
            }
        }
        Event::Deleted { peer, author, target } => {
            if !accept_remote(target, now_ms(), note_to_self(account, &peer)) {
                return;
            }
            let _ = ctx.deleted(&peer.conversation(), &wire::message_id(target, &author));
        }
        // Delete for me has no time limit: the phone already decided, and only this account is told.
        Event::DeletedForMe { peer, author, target } => {
            let _ = ctx.deleted(&peer.conversation(), &wire::message_id(target, &author));
        }
        Event::Forgot(peer) => {
            let _ = ctx.forget_conversation(&peer.conversation());
        }
        Event::Poll { peer, author, name, sent, question, options, multiple, outgoing } => {
            let conversation = peer.conversation();
            if !outgoing
                && (account.live().blocked.contains(&conversation)
                    || account.live().blocked.contains(&Peer::Direct(author.clone()).conversation()))
            {
                return;
            }
            if !outgoing && !name.is_empty() {
                let mut live = account.live();
                if !live.names.contains_key(&author) {
                    live.names.insert(author.clone(), name.clone());
                }
            }
            if !outgoing && matches!(peer, Peer::Direct(_)) {
                let mut live = account.live();
                if !live.accepted.contains(&conversation) {
                    live.requests.insert(conversation.clone());
                }
            }
            match &peer {
                Peer::Direct(aci) => {
                    let title = account.live().names.get(aci).cloned().unwrap_or_default();
                    ensure_row(account, &peer, &title);
                }
                Peer::Group(_) => {
                    if let Ok(None) = ctx.known(&conversation) {
                        let _ = ctx.conversation(&row(&peer, ""));
                    }
                }
            }
            let message = with_poll(account, &conversation, &author, &name, sent, outgoing, Poll::open(question, options, multiple));
            if outgoing {
                account.live().sent(sent, &conversation);
            } else {
                account.live().unread(&conversation, &author, sent);
            }
            let _ = ctx.message(&conversation, &message);
        }
        Event::PollVote { peer, by, author, target, indexes, serial } => {
            let conversation = peer.conversation();
            let id = wire::message_id(target, &author);
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &id) {
                let changed = message.poll.as_mut().is_some_and(|poll| apply_ballot(poll, &by, indexes, serial, &account.me));
                if changed {
                    let _ = ctx.message(&conversation, &message);
                }
            }
        }
        Event::PollEnded { peer, author, target } => {
            let conversation = peer.conversation();
            let id = wire::message_id(target, &author);
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &id) {
                if let Some(poll) = message.poll.as_mut().filter(|poll| !poll.closed) {
                    poll.closed = true;
                    let _ = ctx.message(&conversation, &message);
                }
            }
        }
        Event::Reacted { peer, by, author, target, emoji, removed } => {
            let conversation = peer.conversation();
            let id = wire::message_id(target, &author);
            let mine = by == account.me;
            let replaced = account.live().react(&id, &by, (!removed).then_some(emoji.as_str()));
            if let Some(old) = replaced {
                let _ = ctx.reacted(&conversation, &id, &Reaction { emoji: old, file: None, count: 1, mine, who: vec![by.clone()], ..Reaction::default() }, false);
            }
            let _ = ctx.reacted(&conversation, &id, &Reaction { emoji, file: None, count: 1, mine, who: vec![by], ..Reaction::default() }, !removed);
        }
        Event::ReadElsewhere(read) => read_elsewhere(account, &read),
        Event::Receipt { by, sent, read, viewed, at } => {
            // Every time in the receipt, not only the newest: one receipt can name several messages.
            let at = i64::try_from(at).unwrap_or(0);
            for sent_at in sent {
                if viewed {
                    match story_view_from(account, &by, sent_at) {
                        Some(true) => note_story_viewer(account, sent_at, &by),
                        // Held story, but this person was not in its audience or its group.
                        Some(false) => continue,
                        None => {}
                    }
                }
                file_receipt(account, &by, sent_at, read, viewed, at);
            }
        }
        Event::Typing { peer, by, name, started } => {
            let conversation = peer.conversation();
            {
                let live = account.live();
                if live.blocked.contains(&conversation) || live.blocked.contains(&by) {
                    return;
                }
            }
            if started {
                let name = if name.is_empty() { account.live().name(&by) } else { name };
                ctx.typing(&conversation, &by, &name, TYPING_SHOWN);
            } else {
                ctx.typing_stopped(&conversation, &by);
            }
        }
        Event::Call { peer, by, name, video, id, sent, sdp, opaque } => {
            incoming_call(account, &peer, &by, &name, video, id, sent, sdp, opaque);
        }
        Event::CallAnswer { peer, by, id, sdp, opaque } => {
            call_answered(account, &peer, &by, id, sdp, opaque);
        }
        Event::CallIce { peer, by, id, candidates } => {
            call_ice_received(account, &peer, &by, id, candidates);
        }
        Event::CallOpaque { peer, by, data, urgent } => {
            call_opaque_received(account, &peer, &by, &data, urgent);
        }
        Event::CallEnded { id, answered, kind } => ended_call(account, id, answered, kind),
        Event::GroupCallUpdate { peer, by, era_id, sent } => {
            group_call_update(account, &peer, &by, era_id, sent);
        }
        Event::GroupCallRoster { group_id, info } => {
            apply_group_call_roster(account, &group_id, info);
        }
        Event::GroupCallParticipantState { group_id, aci, muted, camera, sharing, hand_raised, reaction } => {
            group_call_participant_state(account, &group_id, &aci, muted, camera, sharing, hand_raised, reaction);
        }
        Event::CallEvent { peer, call_id, timestamp, kind, incoming, outcome } => {
            apply_call_event(account, &peer, call_id, timestamp, kind, incoming, outcome);
        }
        Event::CallLogEvent { kind, peer, call_id, timestamp } => {
            apply_call_log_event(account, kind, peer.as_ref(), call_id, timestamp);
        }
        Event::CallLinkUpdated { root_key, admin_passkey } => {
            let mut live = account.live();
            let entry = live.call_links.entry(root_key).or_insert_with(|| wire::CallLink {
                root_key,
                url: wire::format_call_link(&root_key),
                name: String::new(),
                approve_all: false,
                admin_passkey: None,
            });
            if admin_passkey.is_some() {
                entry.admin_passkey = admin_passkey;
            }
        }
        Event::Settings { read_receipts, typing, link_previews } => {
            let updated_lp = {
                let mut live = account.live();
                live.read_receipts = read_receipts;
                live.typing = typing;
                if let Some(lp) = link_previews {
                    live.link_previews = lp;
                    Some(lp)
                } else {
                    None
                }
            };
            if let Some(lp) = updated_lp {
                ctx.capabilities(capabilities(lp));
            }
        }
        Event::TimerChanged { peer, seconds, version } => {
            let conversation = peer.conversation();
            if account.live().blocked.contains(&conversation) {
                return;
            }
            account.live().note_timer(&conversation, seconds, version);
            let _ = ctx.disappearing(&conversation, (seconds > 0).then_some(seconds));
            if let Peer::Direct(aci) = &peer {
                let title = account.live().names.get(aci).cloned().unwrap_or_default();
                ensure_row(account, &peer, &title);
            }
            let line = if seconds == 0 {
                "Disappearing messages are off".to_owned()
            } else {
                format!("Disappearing messages are set to {}", duration(seconds))
            };
            let sent_ms = now_ms();
            let mut msg = Message::plain(format!("signal-timer-{sent_ms}"), String::new(), line, (sent_ms / 1_000) as i64, false);
            msg.kind = MessageKind::System;
            let _ = ctx.message(&conversation, &msg);
        }
        Event::ChatStates { chats, pinned, keep_muted_archived } => {
            let mut to_file: HashSet<String> = HashSet::new();
            let mut mutes: Vec<(String, bool)> = Vec::new();
            let mut mark_unreads: Vec<String> = Vec::new();
            let mut relation_updates: Vec<(String, Relation)> = Vec::new();
            {
                let mut live = account.live();
                if let Some(keep) = keep_muted_archived {
                    live.keep_muted_archived = keep;
                }
                if let Some(new_pins) = pinned {
                    for old in &live.pinned {
                        to_file.insert(old.clone());
                    }
                    live.pinned = new_pins.into_iter().map(|p| p.conversation()).collect();
                    for id in &live.pinned {
                        to_file.insert(id.clone());
                    }
                }
                for chat in chats {
                    let conv = chat.peer.conversation();
                    if chat.whitelisted {
                        live.accepted.insert(conv.clone());
                        live.requests.remove(&conv);
                    }
                    if chat.blocked {
                        live.blocked.insert(conv.clone());
                        if let Peer::Direct(aci) = &chat.peer {
                            relation_updates.push((aci.clone(), Relation::Blocked));
                        }
                    } else if live.blocked.remove(&conv) {
                        if let Peer::Direct(aci) = &chat.peer {
                            relation_updates.push((aci.clone(), Relation::Contact));
                        }
                    }
                    if chat.muted_until > 0 {
                        live.muted_until.insert(conv.clone(), chat.muted_until);
                    } else {
                        live.muted_until.remove(&conv);
                    }
                    if chat.dont_notify_for_mentions_if_muted {
                        live.dont_notify_mentions.insert(conv.clone());
                    } else {
                        live.dont_notify_mentions.remove(&conv);
                    }
                    let is_muted = live.is_muted(&conv);
                    mutes.push((conv.clone(), is_muted));
                    let was_archived = live.archived.contains(&conv);
                    if chat.archived {
                        live.archived.insert(conv.clone());
                    } else {
                        live.archived.remove(&conv);
                    }
                    if was_archived != chat.archived || chat.archived {
                        to_file.insert(conv.clone());
                    }
                    if chat.marked_unread {
                        mark_unreads.push(conv);
                    }
                }
            }
            for (person_id, rel) in relation_updates {
                let _ = ctx.relation(&person_id, rel);
            }
            for (conv, muted) in mutes {
                let _ = ctx.muted(&conv, muted);
                if !account.live().folders.is_empty() {
                    to_file.insert(conv);
                }
            }
            for conv in mark_unreads {
                let _ = ctx.mark_unread(&conv);
                if !account.live().folders.is_empty() {
                    to_file.insert(conv);
                }
            }
            for conv in to_file {
                sync_filing(account, &conv);
            }
        }
        Event::StorageSync(snapshot) => apply_storage_sync(account, snapshot),
        Event::Folders(folders) => {
            let (rows, known): (Vec<FolderRow>, Vec<String>) = {
                let mut live = account.live();
                live.folders = folders;
                (live.folder_rows(), live.known_conversations.iter().cloned().collect())
            };
            let _ = ctx.folders(&rows);
            for conv in known {
                sync_filing(account, &conv);
            }
        }
        Event::Blocked { people, groups } => {
            let mut relation_updates: Vec<(String, Relation)> = Vec::new();
            {
                let mut live = account.live();
                let mut next: HashSet<String> = HashSet::new();
                for aci in &people {
                    next.insert(aci.clone());
                }
                for gid in &groups {
                    next.insert(Peer::Group(gid.clone()).conversation());
                }
                for old in live.blocked.difference(&next) {
                    if wire::is_aci(old) {
                        let rel = if live.contacts.contains(old) { Relation::Contact } else { Relation::None };
                        relation_updates.push((old.clone(), rel));
                    }
                }
                for added in &people {
                    relation_updates.push((added.clone(), Relation::Blocked));
                }
                live.blocked = next;
            }
            for (person_id, rel) in relation_updates {
                let _ = ctx.relation(&person_id, rel);
            }
        }
        Event::MessageRequestResponse { peer, action } => {
            let conv = peer.conversation();
            match action {
                wire::MessageRequestAction::Accept => {
                    let title = {
                        let mut live = account.live();
                        live.requests.remove(&conv);
                        live.accepted.insert(conv.clone());
                        live.names.get(&conv).cloned().unwrap_or_default()
                    };
                    ensure_row(account, &peer, &title);
                }
                wire::MessageRequestAction::Delete | wire::MessageRequestAction::Spam => {
                    account.live().requests.remove(&conv);
                    let _ = ctx.forget_conversation(&conv);
                }
                wire::MessageRequestAction::Block => {
                    {
                        let mut live = account.live();
                        live.requests.remove(&conv);
                        live.blocked.insert(conv.clone());
                    }
                    if let Peer::Direct(aci) = &peer {
                        let _ = ctx.relation(aci, Relation::Blocked);
                    }
                }
                wire::MessageRequestAction::BlockAndDelete | wire::MessageRequestAction::BlockAndSpam => {
                    {
                        let mut live = account.live();
                        live.requests.remove(&conv);
                        live.blocked.insert(conv.clone());
                    }
                    if let Peer::Direct(aci) = &peer {
                        let _ = ctx.relation(aci, Relation::Blocked);
                    }
                    let _ = ctx.forget_conversation(&conv);
                }
            }
        }
        Event::IdentityChanged { peer } => {
            if let Peer::Direct(aci) = peer {
                let (name, was_verified, shared_groups) = {
                    let mut live = account.live();
                    let was_verified = live.identity_states.get(&aci) == Some(&wire::IdentityState::Verified);
                    if was_verified {
                        live.identity_states.insert(aci.clone(), wire::IdentityState::Unverified);
                        live.identity_changed.insert(aci.clone());
                    }
                    let name = live.name(&aci);
                    let shared: Vec<String> = live
                        .groups
                        .iter()
                        .filter(|(_, members)| members.iter().any(|m| m == &aci))
                        .map(|(conv, _)| conv.clone())
                        .collect();
                    (name, was_verified, shared)
                };
                if was_verified {
                    let _ = ctx.details(&[(aci.clone(), PersonDetails { mark: None, ..PersonDetails::default() })]);
                }
                let line = format!("Your safety number with {name} has changed");
                let sent_ms = now_ms();
                let sent_s = (sent_ms / 1_000) as i64;
                let direct_title = account.live().known_name(&aci);
                ensure_row(account, &Peer::Direct(aci.clone()), &direct_title);
                let mut dm_msg = Message::plain(format!("signal-identity-{sent_ms}-{aci}"), String::new(), line.clone(), sent_s, false);
                dm_msg.kind = MessageKind::System;
                let _ = ctx.message(&aci, &dm_msg);
                for conv in shared_groups {
                    let mut grp_msg = Message::plain(format!("signal-identity-{sent_ms}-{aci}-{conv}"), String::new(), line.clone(), sent_s, false);
                    grp_msg.kind = MessageKind::System;
                    let _ = ctx.message(&conv, &grp_msg);
                }
            }
        }
        Event::VerifiedChanged { aci, state } => {
            {
                let mut live = account.live();
                live.identity_states.insert(aci.clone(), state);
                live.identity_changed.remove(&aci);
            }
            let mark = (state == wire::IdentityState::Verified).then_some(Mark::Verified);
            let _ = ctx.details(&[(aci, PersonDetails { mark, ..PersonDetails::default() })]);
        }
        Event::Pinned { peer, by, author, target, duration_secs } => {
            if target == 0 || author.is_empty() {
                return;
            }
            let conv = peer.conversation();
            {
                let live = account.live();
                if live.blocked.contains(&conv) {
                    return;
                }
                if matches!(peer, Peer::Group(_))
                    && live.group_info.get(&conv).is_some_and(|g| !group_rights(g, &by).pin)
                {
                    return;
                }
            }
            let msg_id = wire::message_id(target, &author);
            let now = crate::hub::now();
            let expires_at = duration_secs.filter(|&s| s > 0).map(|s| now + i64::from(s));
            let by_name = if by == account.me { "You".to_owned() } else { account.live().name(&by) };
            account.live().record_pin(&conv, &msg_id, by_name.clone(), now, expires_at);
            let _ = ctx.pinned(&conv, &msg_id, true);
            if let Ok(Some(mut stored)) = ctx.stored_message(&conv, &msg_id) {
                stored.pinned = true;
                stored.pinned_by = Some(by_name.clone());
                stored.pinned_at = Some(now);
                let _ = ctx.message(&conv, &stored);
            }
            let sent_ms = now_ms();
            let mut sys = Message::plain(
                format!("signal-pin-{sent_ms}-{target}"),
                String::new(),
                format!("{by_name} pinned a message"),
                (sent_ms / 1_000) as i64,
                by == account.me,
            );
            sys.kind = MessageKind::System;
            let _ = ctx.message(&conv, &sys);
        }
        Event::Unpinned { peer, by, author, target } => {
            if target == 0 || author.is_empty() {
                return;
            }
            let conv = peer.conversation();
            if matches!(peer, Peer::Group(_))
                && account.live().group_info.get(&conv).is_some_and(|g| !group_rights(g, &by).pin)
            {
                return;
            }
            let msg_id = wire::message_id(target, &author);
            account.live().remove_pin(&conv, &msg_id);
            let _ = ctx.pinned(&conv, &msg_id, false);
            if let Ok(Some(mut stored)) = ctx.stored_message(&conv, &msg_id) {
                stored.pinned = false;
                stored.pinned_by = None;
                stored.pinned_at = None;
                let _ = ctx.message(&conv, &stored);
            }
        }
        Event::StickerPacks(packs) => {
            let mut live = account.live();
            if !live.stickers_loaded {
                live.sticker_packs = read_sticker_packs(&live.stickers_path);
                live.stickers_loaded = true;
            }
            for pack in packs {
                if let Some(existing) = live.sticker_packs.iter_mut().find(|p| p.id == pack.id) {
                    *existing = pack;
                } else if live.sticker_packs.len() < 128 {
                    live.sticker_packs.push(pack);
                }
            }
            write_sticker_packs(&live.stickers_path, &live.sticker_packs);
            account.ctx.hub.catalogs.forget_matching(|(acct, kind, ..)| *acct == account.ctx.account && *kind == CatalogKind::Stickers);
        }
        Event::StickerPackOp { pack_id, pack_key, install } => {
            if install {
                let mut pack = account
                    .wire
                    .preview_sticker_pack(&pack_id, &pack_key)
                    .unwrap_or_else(|_| wire::StickerPackInfo {
                        id: pack_id.clone(),
                        key: pack_key.clone(),
                        title: "Sticker pack".to_owned(),
                        author: String::new(),
                        cover_id: Some(0),
                        stickers: Vec::new(),
                        installed: true,
                    });
                pack.installed = true;
                let mut live = account.live();
                if !live.stickers_loaded {
                    live.sticker_packs = read_sticker_packs(&live.stickers_path);
                    live.stickers_loaded = true;
                }
                if let Some(existing) = live.sticker_packs.iter_mut().find(|p| p.id == pack.id) {
                    *existing = pack;
                } else if live.sticker_packs.len() < 128 {
                    live.sticker_packs.push(pack);
                }
                write_sticker_packs(&live.stickers_path, &live.sticker_packs);
            } else {
                let mut live = account.live();
                if !live.stickers_loaded {
                    live.sticker_packs = read_sticker_packs(&live.stickers_path);
                    live.stickers_loaded = true;
                }
                live.sticker_packs.retain(|p| p.id != pack_id);
                write_sticker_packs(&live.stickers_path, &live.sticker_packs);
            }
            account.ctx.hub.catalogs.forget_matching(|(acct, kind, ..)| *acct == account.ctx.account && *kind == CatalogKind::Stickers);
        }
        Event::Story(story) => apply_story(account, story),
        Event::StoryReaction { by, story_author, story_sent, emoji, removed, .. } => {
            if account.live().stories_disabled {
                return;
            }
            let story_id = wire::message_id(story_sent, &story_author);
            if visible_story(account, &story_id).is_none() {
                return;
            }
            let mine = by == account.me;
            let prior = account.live().react(&story_id, &by, (!removed).then_some(emoji.as_str()));
            if let Some(old) = prior {
                let _ = ctx.reacted(
                    STORIES_CONVERSATION,
                    &story_id,
                    &Reaction { emoji: old, file: None, count: 1, mine, who: vec![by.clone()], burst: 0, mine_burst: false, burst_colors: Vec::new() },
                    false,
                );
            }
            let _ = ctx.reacted(
                STORIES_CONVERSATION,
                &story_id,
                &Reaction { emoji, file: None, count: 1, mine, who: vec![by], burst: 0, mine_burst: false, burst_colors: Vec::new() },
                !removed,
            );
        }
        Event::StoriesSettings { disabled, view_receipts } => {
            {
                let mut live = account.live();
                live.stories_disabled = disabled;
                live.story_view_receipts = view_receipts;
                if disabled {
                    live.stories.clear();
                }
            }
            if disabled {
                let _ = ctx.forget_conversation(STORIES_CONVERSATION);
            }
        }
        Event::Caught => {
            prune_expired_pins(account);
            prune_expired_stories(account);
        }
        Event::Closed(_) | Event::Ended(_) => {}
    }
}

fn apply_storage_sync(account: &Account, snapshot: wire::StorageSnapshot) {
    let ctx = &account.ctx;
    let mut to_file: HashSet<String> = HashSet::new();
    let mut mutes: Vec<(String, bool)> = Vec::new();
    let mut mark_unreads: Vec<String> = Vec::new();
    let mut relation_updates: Vec<(String, Relation)> = Vec::new();
    let mut mark_updates: Vec<(String, Option<Mark>)> = Vec::new();
    let mut people_rows: Vec<PersonRow> = Vec::new();
    let mut seed_rows: Vec<(Peer, String)> = Vec::new();
    let mut updated_link_previews: Option<bool> = None;
    let mut disabled_stories = false;
    {
        let mut live = account.live();
        if !snapshot.distribution_lists.is_empty() {
            for list in snapshot.distribution_lists {
                if let Some(existing) = live.story_lists.iter_mut().find(|l| l.id == list.id) {
                    *existing = list;
                } else if live.story_lists.len() < 64 {
                    live.story_lists.push(list);
                }
            }
        }
        if let Some(acct) = snapshot.account {
            if let Some(disabled) = acct.stories_disabled {
                live.stories_disabled = disabled;
                if disabled {
                    live.stories.clear();
                    disabled_stories = true;
                }
            }
            if let Some(receipts) = acct.story_view_receipts {
                live.story_view_receipts = receipts;
            }
            if let Some(keep) = acct.keep_muted_archived {
                live.keep_muted_archived = keep;
            }
            if let Some(lp) = acct.link_previews {
                live.link_previews = lp;
                updated_link_previews = Some(lp);
            }
            live.default_timer = acct.universal_expire_timer;
            for old in &live.pinned {
                to_file.insert(old.clone());
            }
            live.pinned = acct
                .pinned
                .into_iter()
                .filter_map(|id| Peer::parse(&id).map(|p| p.conversation()))
                .collect();
            for id in &live.pinned {
                to_file.insert(id.clone());
                if let Some(peer) = Peer::parse(id) {
                    let title = match &peer {
                        Peer::Direct(aci) => live.known_name(aci),
                        Peer::Group(_) => live.group_info.get(id).map(|g| g.title.clone()).unwrap_or_default(),
                    };
                    seed_rows.push((peer, title));
                }
            }
            if acct.note_to_self_marked_unread {
                mark_unreads.push(account.me.clone());
            }
        }
        for contact in snapshot.contacts {
            if !wire::is_aci(&contact.aci) {
                continue;
            }
            let aci = contact.aci;
            if !contact.name.is_empty() {
                live.names.insert(aci.clone(), contact.name);
            }
            if !contact.nickname.is_empty() {
                live.nicknames.insert(aci.clone(), contact.nickname);
            } else {
                live.nicknames.remove(&aci);
            }
            if !contact.note.is_empty() {
                live.notes.insert(aci.clone(), contact.note);
            } else {
                live.notes.remove(&aci);
            }
            if !contact.phone.is_empty() {
                live.phones.insert(aci.clone(), contact.phone);
            }
            if !contact.username.is_empty() {
                live.usernames.insert(aci.clone(), contact.username);
            }
            live.identity_states.insert(aci.clone(), contact.identity_state);
            if contact.identity_state == wire::IdentityState::Verified {
                live.identity_changed.remove(&aci);
                mark_updates.push((aci.clone(), Some(Mark::Verified)));
            } else {
                mark_updates.push((aci.clone(), None));
            }
            live.contacts.insert(aci.clone());
            if contact.whitelisted {
                live.accepted.insert(aci.clone());
                live.requests.remove(&aci);
            }
            if contact.blocked {
                live.blocked.insert(aci.clone());
                relation_updates.push((aci.clone(), Relation::Blocked));
            } else {
                live.blocked.remove(&aci);
                relation_updates.push((aci.clone(), Relation::Contact));
            }
            if contact.muted_until > 0 {
                live.muted_until.insert(aci.clone(), contact.muted_until);
            } else {
                live.muted_until.remove(&aci);
            }
            let is_muted = live.is_muted(&aci);
            mutes.push((aci.clone(), is_muted));
            let was_archived = live.archived.contains(&aci);
            if contact.archived {
                live.archived.insert(aci.clone());
            } else {
                live.archived.remove(&aci);
            }
            if was_archived != contact.archived || contact.archived {
                to_file.insert(aci.clone());
            }
            if contact.marked_unread {
                mark_unreads.push(aci.clone());
            }
            let display = live.known_name(&aci);
            let handle = live.usernames.get(&aci).cloned().unwrap_or_default();
            people_rows.push(PersonRow {
                id: aci.clone(),
                name: if display.is_empty() { NO_NAME.to_owned() } else { display.clone() },
                handle,
                avatar: ctx.avatar_of(&aci),
                bot: false,
                status: String::new(),
                link: String::new(),
            });
            if !contact.hidden {
                seed_rows.push((Peer::Direct(aci), display));
            }
        }
        for group in snapshot.groups {
            let peer = Peer::Group(group.id);
            let conv = peer.conversation();
            if group.whitelisted {
                live.accepted.insert(conv.clone());
                live.requests.remove(&conv);
            }
            if group.blocked {
                live.blocked.insert(conv.clone());
            } else {
                live.blocked.remove(&conv);
            }
            if group.muted_until > 0 {
                live.muted_until.insert(conv.clone(), group.muted_until);
            } else {
                live.muted_until.remove(&conv);
            }
            if group.dont_notify_for_mentions_if_muted {
                live.dont_notify_mentions.insert(conv.clone());
            } else {
                live.dont_notify_mentions.remove(&conv);
            }
            let is_muted = live.is_muted(&conv);
            mutes.push((conv.clone(), is_muted));
            let was_archived = live.archived.contains(&conv);
            if group.archived {
                live.archived.insert(conv.clone());
            } else {
                live.archived.remove(&conv);
            }
            if was_archived != group.archived || group.archived {
                to_file.insert(conv.clone());
            }
            if group.marked_unread {
                mark_unreads.push(conv.clone());
            }
            let title = if !group.title.is_empty() {
                group.title
            } else {
                live.group_info.get(&conv).map(|g| g.title.clone()).unwrap_or_default()
            };
            seed_rows.push((peer, title));
        }
    }
    if !people_rows.is_empty() {
        let _ = ctx.people(&people_rows);
    }
    for (person_id, rel) in relation_updates {
        let _ = ctx.relation(&person_id, rel);
    }
    if !mark_updates.is_empty() {
        let details: Vec<(String, PersonDetails)> = mark_updates
            .into_iter()
            .map(|(id, mark)| (id, PersonDetails { mark, ..PersonDetails::default() }))
            .collect();
        let _ = ctx.details(&details);
    }
    for (peer, title) in seed_rows {
        let conv = peer.conversation();
        ensure_row(account, &peer, &title);
        if !account.live().folders.is_empty() {
            to_file.insert(conv);
        }
    }
    for (conv, muted) in mutes {
        let _ = ctx.muted(&conv, muted);
        if !account.live().folders.is_empty() {
            to_file.insert(conv);
        }
    }
    for conv in mark_unreads {
        let _ = ctx.mark_unread(&conv);
        if !account.live().folders.is_empty() {
            to_file.insert(conv);
        }
    }
    for conv in to_file {
        sync_filing(account, &conv);
    }
    if let Some(lp) = updated_link_previews {
        ctx.capabilities(capabilities(lp));
    }
    if disabled_stories {
        let _ = ctx.forget_conversation(STORIES_CONVERSATION);
    }
}

fn ensure_stories_row(account: &Account) {
    if matches!(account.ctx.known(STORIES_CONVERSATION), Ok(Some(_))) {
        return;
    }
    let mut row = ConversationRow::new(STORIES_CONVERSATION.to_owned(), ConversationKind::Channel, "Stories");
    row.listed = true;
    row.private = true;
    row.topic = "Active Signal stories expire after 24 hours".to_owned();
    let _ = account.ctx.conversation(&row);
}

fn prune_expired_stories(account: &Account) {
    let now_secs = crate::hub::now();
    let mut expired = Vec::new();
    account.live().stories.retain(|id, story| {
        if story.expires_at <= now_secs {
            expired.push(id.clone());
            false
        } else {
            true
        }
    });
    for id in expired {
        let _ = account.ctx.deleted(STORIES_CONVERSATION, &id);
    }
}

fn apply_story(account: &Account, story: wire::StoryMessageData) {
    {
        let live = account.live();
        if live.stories_disabled || live.blocked.contains(&story.author) {
            return;
        }
        if let Some(gid) = &story.group_id {
            if live.blocked.contains(&Peer::Group(gid.clone()).conversation()) {
                return;
            }
        }
    }
    let sent_secs = i64::try_from(story.sent / 1_000).unwrap_or(0);
    let expires_at = sent_secs.saturating_add(STORY_LIFETIME_SECS);
    if expires_at <= crate::hub::now() {
        return;
    }
    ensure_stories_row(account);
    let msg_id = wire::message_id(story.sent, &story.author);
    let author_name = if story.outgoing || story.author == account.me {
        account.name.clone()
    } else if !story.name.is_empty() {
        story.name.clone()
    } else {
        account.live().name(&story.author)
    };
    let group_label = story.group_id.as_deref().map(|gid| {
        let conv = Peer::Group(gid.to_owned()).conversation();
        account
            .live()
            .group_info
            .get(&conv)
            .map(|g| g.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Group story".to_owned())
    });

    let mut body_text = String::new();
    let mut snippet = String::new();
    let mut msg = Message::plain(msg_id.clone(), author_name, "", sent_secs, story.outgoing);
    msg.sender.id = story.author.clone();
    msg.expires_at = Some(expires_at);

    if let Some(st) = &story.text {
        body_text = st.text.clone();
        snippet = st.text.clone();
        msg.text = body_text;
        msg.card = Some(Box::new(protocol::Card {
            author: group_label.unwrap_or_else(|| "Story".to_owned()),
            title: String::new(),
            text: st.text.clone(),
            color: Some(st.bg_color & 0x00ff_ffff),
            footer: if story.allows_replies {
                "Story · replies allowed".to_owned()
            } else {
                "Story · replies off".to_owned()
            },
            ..protocol::Card::default()
        }));
        if let Some(preview) = &st.preview {
            if wire::story_preview_kept(&st.text, &preview.url) {
                msg.previews.push(protocol::Preview {
                    duration: 0,
                    opens_in_browser: false,
                    url: preview.url.clone(),
                    site: crate::adapters::tiny::preview::real_host(&preview.url),
                    title: preview.title.clone(),
                    description: preview.description.clone(),
                    image: None,
                });
            }
        }
    } else if let Some(ptr) = &story.file {
        let kind = files::attachment_kind(&ptr.content_type, ptr.flags);
        let file_id = format!("signal:story:{}:{}", story.sent, story.author);
        let address = format!("signal://attachment/{file_id}");
        let mut mf = MediaFile::new(file_id.clone(), kind);
        mf.width = ptr.width;
        mf.height = ptr.height;
        mf.bytes = u64::from(ptr.size);
        let _ = account.ctx.media(&[(mf.clone(), address.clone())]);
        account.live().remember_attachment(
            file_id,
            HeldAttachment {
                pointer: ptr.clone(),
                file: mf.clone(),
                address,
                conversation: STORIES_CONVERSATION.to_owned(),
                message_id: msg_id.clone(),
            },
        );
        let name = if !ptr.file_name.is_empty() {
            ptr.file_name.clone()
        } else {
            files::attachment_label(&ptr.content_type, "", ptr.flags)
        };
        snippet = if !ptr.caption.is_empty() { ptr.caption.clone() } else { name.clone() };
        msg.text = ptr.caption.clone();
        let mut att = Attachment::new(name, mf);
        att.mime = ptr.content_type.clone();
        msg.attachments.push(att);
    }

    {
        let mut live = account.live();
        let (audience, viewed_by) = live
            .stories
            .get(&msg_id)
            .map(|old| (old.audience.clone(), old.viewed_by.clone()))
            .unwrap_or_default();
        live.stories.insert(
            msg_id.clone(),
            HeldStory {
                author: story.author,
                sent: story.sent,
                group_id: story.group_id,
                allows_replies: story.allows_replies,
                snippet,
                expires_at,
                audience,
                viewed_by,
            },
        );
    }
    let _ = account.ctx.message(STORIES_CONVERSATION, &msg);
}

/// The user read messages on another device: the rows they were in are read up to there.
fn read_elsewhere(account: &Account, read: &[(String, u64)]) {
    let mut touched: Vec<(String, u64, String)> = Vec::new();
    {
        let mut live = account.live();
        let held: Vec<(String, u64, String)> = live
            .unread
            .iter()
            .flat_map(|(conversation, held)| {
                read.iter()
                    .filter(|(author, sent)| held.iter().any(|(a, s)| a == author && s == sent))
                    .map(move |(author, sent)| (conversation.clone(), *sent, author.clone()))
            })
            .collect();
        for (conversation, sent, author) in &held {
            match touched.iter_mut().find(|(c, _, _)| c == conversation) {
                Some(seen) if seen.1 < *sent => *seen = (conversation.clone(), *sent, author.clone()),
                Some(_) => {}
                None => touched.push((conversation.clone(), *sent, author.clone())),
            }
        }
        for (conversation, upto, _) in &touched {
            live.read_upto(conversation, *upto);
        }
        let mut to_expire = Vec::new();
        for (conversation, sent, author) in held {
            let msg_id = wire::message_id(sent, &author);
            if let Some(secs) = live.take_expiring(&conversation, &msg_id) {
                to_expire.push((conversation, msg_id, secs));
            }
        }
        drop(live);
        for (conversation, msg_id, secs) in to_expire {
            start_expiry(account, &conversation, &msg_id, secs);
        }
    }
    for (conversation, upto, author) in touched {
        let left = account.live().unread.get(&conversation).map_or(0, VecDeque::len) as u32;
        let updated = account.ctx.known(&conversation).ok().flatten().map_or(0, |row| row.updated);
        let _ = account.ctx.counts(&conversation, left, 0, Some(&wire::message_id(upto, &author)), updated);
        if !account.live().folders.is_empty() {
            sync_filing(account, &conversation);
        }
    }
}

/// Whether an offer sent `sent` (ms) is recent enough to ring for. One from before we
/// connected is a call that is long over; allow small clock skew from remote phones.
fn fresh_call(sent: u64) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    sent <= now.saturating_add(5_000) && now.saturating_sub(sent) <= (CALL_STALE as u64) * 1_000
}

/// Someone is calling: their row is recorded (a ring names a conversation the list may not
/// have yet), and the ring is presented natively in tinyclient's call dock + notification
/// with Accept and Decline. If we are already in another call, reply with Signal's `Busy`
/// message. An offer that waited while we were away is remembered for its end, not rung.
#[allow(clippy::too_many_arguments)]
fn incoming_call(
    account: &Account,
    peer: &Peer,
    by: &str,
    name: &str,
    video: bool,
    id: u64,
    sent: u64,
    sdp: String,
    opaque: Vec<u8>,
) {
    if by == account.me || !matches!(peer, Peer::Direct(_)) {
        return;
    }
    let conversation = peer.conversation();
    if account.live().blocked.contains(&conversation) || account.live().blocked.contains(&Peer::Direct(by.to_owned()).conversation()) {
        return;
    }
    let caller = if name.is_empty() { account.live().name(by) } else { name.to_owned() };
    ensure_row(account, peer, &caller);
    // Already in an active call on another conversation: answer Busy per Signal protocol.
    if account.live().active_call.as_ref().is_some_and(|c| c.id != id) {
        let _ = account.wire.send(peer, Body::Call(CallSignal::Busy { id }));
        return;
    }
    // The newest offer takes the one ring slot. An older or replayed one leaves both the slot
    // and the ring alone: a displaced call's hangup matches nothing here, and a losing offer
    // must not ring for a note its own hangup could not end.
    if account.live().ring.as_ref().is_none_or(|held| held.sent < sent) {
        account.live().ring = Some(Ring {
            id,
            conversation: conversation.clone(),
            author: by.to_owned(),
            video,
            sent,
            sdp,
            opaque,
            pending_ice: Vec::new(),
        });
        if fresh_call(sent) {
            account.ctx.ring(&conversation, &caller);
        }
    }
}

/// Builds `CallSecurity` from Signal's 60-digit safety number and identity verification state
/// for a 1:1 call with `peer_aci`.
fn signal_call_security(account: &Account, peer_aci: &str) -> CallSecurity {
    let safety = account.wire.safety_number(peer_aci).ok().flatten();
    let (code, trust, key) = match safety {
        Some(sn) => {
            let trust = if sn.changed {
                Trust::Changed
            } else {
                match sn.state {
                    wire::IdentityState::Verified => Trust::Verified,
                    wire::IdentityState::Default | wire::IdentityState::Unverified => Trust::Unverified,
                }
            };
            let key = sn.digits.chars().take(16).collect();
            (sn.digits, trust, key)
        }
        None => {
            let live = account.live();
            let trust = if live.identity_changed.contains(peer_aci) {
                Trust::Changed
            } else if live.identity_states.get(peer_aci) == Some(&wire::IdentityState::Verified) {
                Trust::Verified
            } else {
                Trust::Unverified
            };
            (String::new(), trust, String::new())
        }
    };
    CallSecurity {
        mode: SecurityMode::EndToEnd,
        code: code.clone(),
        emojis: Vec::new(),
        epoch: 1,
        members: vec![SecureMember { user_id: peer_aci.to_owned(), key, safety: code, trust }],
        unsupported: Vec::new(),
        server_lacks: false,
        required: true,
        identity: String::new(),
        stream: false,
    }
}

/// Fetches Signal's TURN relays and maps them into `mediad`'s `SignalRelay` list.
fn signal_relays(account: &Account) -> Vec<SignalRelay> {
    account
        .wire
        .turn_servers()
        .unwrap_or_default()
        .into_iter()
        .map(|t| SignalRelay { urls: t.urls, username: t.username, password: t.password })
        .collect()
}

/// Starts an outgoing 1:1 Signal call or answers an incoming ring, or starts/joins a Signal group call (`Command::CallJoin`).
fn call_join(account: &Account, conversation_id: &str) {
    let Ok(target_peer) = peer(conversation_id) else {
        account.ctx.call_failed(conversation_id, CallFailure::Refused, "That Signal conversation can't be called.");
        return;
    };
    let peer_aci = match target_peer.clone() {
        Peer::Direct(aci) => aci,
        Peer::Group(group_id) => {
            group_call_join(account, conversation_id, &group_id);
            return;
        }
    };

    let always_relay = account.ctx.hub.settings().on(protocol::settings::SIGNAL_ALWAYS_RELAY);
    let is_contact = account.live().contacts.contains(&peer_aci);
    let relay_only = protocol::settings::signal_relay_only(is_contact, always_relay);
    let relays = signal_relays(account);
    let security = signal_call_security(account, &peer_aci);
    let peer_name = account.live().name(&peer_aci);

    // Check if we are answering an incoming ring for this conversation.
    let incoming_ring = {
        let mut live = account.live();
        if live.ring.as_ref().is_some_and(|r| r.conversation == conversation_id) {
            live.ring.take()
        } else {
            None
        }
    };

    if let Some(ring) = incoming_ring {
        let answer_sdp = if ring.sdp.starts_with("v=0") {
            let accept = SignalAccept {
                user_id: account.me.clone(),
                peer_id: peer_aci.clone(),
                host: "127.0.0.1".to_owned(),
                offer_sdp: ring.sdp.clone(),
                video: ring.video,
                relay_only,
                relays,
                muted: false,
                deafened: false,
            };
            match account.ctx.signal_accept(conversation_id, accept) {
                Ok(sdp) => sdp,
                Err(detail) => {
                    account.ctx.call_failed(conversation_id, CallFailure::Engine, &detail);
                    return;
                }
            }
        } else {
            // Scripted / opaque-only offer: generate an answer SDP locally via mediad offer if possible.
            let offer = SignalOffer {
                user_id: account.me.clone(),
                peer_id: peer_aci.clone(),
                host: "127.0.0.1".to_owned(),
                video: ring.video,
                relay_only,
                relays,
                muted: false,
                deafened: false,
            };
            account.ctx.signal_offer(conversation_id, offer).unwrap_or_default()
        };

        let active = ActiveSignalCall {
            id: ring.id,
            conversation: conversation_id.to_owned(),
            peer_aci: peer_aci.clone(),
            video: ring.video,
            outgoing: false,
            answered: true,
            muted: false,
            deafened: false,
            camera: ring.video,
            sharing: false,
            pending_ice: Vec::new(),
            sent_ms: ring.sent,
        };
        account.live().active_call = Some(active);

        if let Err(fault) = account.wire.send(
            &target_peer,
            Body::Call(CallSignal::Answer {
                id: ring.id,
                sdp: answer_sdp,
                opaque: Vec::new(),
            }),
        ) {
            account.live().active_call = None;
            account.ctx.call_failed(conversation_id, CallFailure::Lost, &fault.words());
            return;
        }
        for cand in ring.pending_ice {
            account.ctx.signal_ice(conversation_id, cand);
        }
        account.ctx.call_security(conversation_id, Some(security));
        account.ctx.participants(
            conversation_id,
            vec![Participant {
                id: peer_aci,
                name: peer_name,
                camera: ring.video,
                ..Participant::default()
            }],
        );
        // If there is no real SDP from a remote peer (scripted fake wire), mark joined immediately
        // once Answer is accepted by the wire; with a real remote peer, mediad Connected fires.
        if !ring.sdp.starts_with("v=0") {
            account.ctx.call_state(conversation_id, CallState::Joined);
        }
        return;
    }

    // Outgoing 1:1 call: enter RingingOut, generate str0m SDP offer, send OfferMessage over Signal.
    let call_id = now_ms();
    account.ctx.ringing_out(conversation_id);
    let offer_spec = SignalOffer {
        user_id: account.me.clone(),
        peer_id: peer_aci.clone(),
        host: "127.0.0.1".to_owned(),
        video: false,
        relay_only,
        relays,
        muted: false,
        deafened: false,
    };
    let sdp = match account.ctx.signal_offer(conversation_id, offer_spec) {
        Ok(sdp) => sdp,
        Err(detail) => {
            account.ctx.call_failed(conversation_id, CallFailure::Engine, &detail);
            return;
        }
    };
    account.live().active_call = Some(ActiveSignalCall {
        id: call_id,
        conversation: conversation_id.to_owned(),
        peer_aci: peer_aci.clone(),
        video: false,
        outgoing: true,
        answered: false,
        muted: false,
        deafened: false,
        camera: false,
        sharing: false,
        pending_ice: Vec::new(),
        sent_ms: call_id,
    });
    if let Err(fault) = account.wire.send(
        &target_peer,
        Body::Call(CallSignal::Offer {
            id: call_id,
            video: false,
            sdp,
            opaque: Vec::new(),
        }),
    ) {
        account.live().active_call = None;
        account.ctx.call_failed(conversation_id, CallFailure::Lost, &fault.words());
        return;
    }
    account.ctx.call_security(conversation_id, Some(security));
}

/// Handles `CallMessage.answer` for our outgoing 1:1 call.
fn call_answered(account: &Account, _peer: &Peer, by: &str, id: u64, sdp: String, _opaque: Vec<u8>) {
    let matched = {
        let mut live = account.live();
        if let Some(active) = live.active_call.as_mut() {
            let same = active.outgoing && active.peer_aci == by && (id == 0 || active.id == id);
            if same {
                active.answered = true;
                let buffered = std::mem::take(&mut active.pending_ice);
                Some((active.conversation.clone(), active.peer_aci.clone(), active.video, buffered))
            } else {
                None
            }
        } else {
            None
        }
    };
    let Some((conversation, peer_aci, video, buffered_ice)) = matched else {
        return;
    };
    let peer_name = account.live().name(&peer_aci);
    account.ctx.answered(&conversation);
    account.ctx.participants(
        &conversation,
        vec![Participant {
            id: peer_aci,
            name: peer_name,
            camera: video,
            ..Participant::default()
        }],
    );
    if sdp.starts_with("v=0") {
        if let Err(detail) = account.ctx.signal_answer(&conversation, sdp) {
            account.ctx.call_failed(&conversation, CallFailure::Engine, &detail);
            return;
        }
        for cand in buffered_ice {
            account.ctx.signal_ice(&conversation, cand);
        }
    } else {
        for cand in buffered_ice {
            account.ctx.signal_ice(&conversation, cand);
        }
        account.ctx.call_state(&conversation, CallState::Joined);
    }
}

/// Handles incoming `CallMessage.iceUpdate` candidates from the remote peer.
fn call_ice_received(account: &Account, _peer: &Peer, by: &str, id: u64, candidates: Vec<String>) {
    let active_conv = {
        let mut live = account.live();
        if let Some(active) = live.active_call.as_mut() {
            if active.peer_aci == by && (id == 0 || active.id == id) {
                if active.answered {
                    Some(active.conversation.clone())
                } else {
                    active.pending_ice.extend(candidates);
                    return;
                }
            } else {
                None
            }
        } else if let Some(ring) = live.ring.as_mut() {
            if ring.author == by && (id == 0 || ring.id == id) {
                ring.pending_ice.extend(candidates);
            }
            return;
        } else {
            None
        }
    };
    if let Some(conversation) = active_conv {
        for cand in candidates {
            account.ctx.signal_ice(&conversation, cand);
        }
    }
}

/// Sends a local ICE candidate from `mediad` to the remote peer via `CallMessage.iceUpdate`.
fn call_ice_send(account: &Account, conversation_id: &str, candidate: String) {
    let target = {
        let live = account.live();
        live.active_call
            .as_ref()
            .filter(|c| c.conversation == conversation_id)
            .map(|c| (c.id, c.peer_aci.clone()))
    };
    let Some((id, peer_aci)) = target else {
        return;
    };
    let _ = account.wire.send(
        &Peer::Direct(peer_aci),
        Body::Call(CallSignal::IceUpdate { id, candidates: vec![candidate] }),
    );
}

/// Leaves, cancels, or declines a Signal 1:1 or group call (`Command::CallLeave`).
fn call_leave(account: &Account, conversation_id: &str) {
    if conversation_id.starts_with("g.") {
        group_call_leave(account, conversation_id);
        return;
    }
    // Declining an incoming ring before accepting.
    let declined_ring = {
        let mut live = account.live();
        if live.ring.as_ref().is_some_and(|r| r.conversation == conversation_id) {
            live.ring.take()
        } else {
            None
        }
    };
    if let Some(ring) = declined_ring {
        account.ctx.ring_stopped(&ring.conversation);
        let _ = account.wire.send(
            &Peer::Direct(ring.author),
            Body::Call(CallSignal::Hangup { id: ring.id, kind: HangupKind::Declined, device_id: None }),
        );
        return;
    }

    // Hanging up or cancelling an active outgoing/joined call.
    let ended = {
        let mut live = account.live();
        if live.active_call.as_ref().is_some_and(|c| c.conversation == conversation_id) {
            live.active_call.take()
        } else {
            None
        }
    };
    if let Some(call) = ended {
        let _ = account.wire.send(
            &Peer::Direct(call.peer_aci),
            Body::Call(CallSignal::Hangup { id: call.id, kind: HangupKind::Normal, device_id: None }),
        );
    }
}

/// The call ended: the ring or active call goes, and a call nobody answered here leaves the line
/// the other networks leave. One answered on another device (`HangupKind::Accepted`) leaves none.
fn ended_call(account: &Account, id: u64, answered: bool, kind: HangupKind) {
    // Check if this ends our active call.
    let active = {
        let mut live = account.live();
        if live.active_call.as_ref().is_some_and(|c| id == 0 || c.id == id) {
            live.active_call.take()
        } else {
            None
        }
    };
    if let Some(call) = active {
        if !call.answered && matches!(kind, HangupKind::Busy | HangupKind::Declined) {
            let detail = if kind == HangupKind::Busy {
                "Busy on another call."
            } else {
                "Call declined."
            };
            account.ctx.call_failed(&call.conversation, CallFailure::Refused, detail);
        } else {
            account.ctx.call_over(&call.conversation);
        }
        return;
    }

    if account.live().ring.as_ref().is_none_or(|ring| ring.id != id) {
        return;
    }
    let Some(ring) = account.live().ring.take() else {
        return;
    };
    account.ctx.ring_stopped(&ring.conversation);
    if answered || kind == HangupKind::Accepted {
        return;
    }
    let mut line = Message::plain(
        wire::message_id(ring.sent, &ring.author),
        account.live().name(&ring.author),
        if ring.video { "Missed video call" } else { "Missed voice call" },
        i64::try_from(ring.sent / 1_000).unwrap_or(0),
        false,
    );
    line.kind = MessageKind::System;
    line.sender.id = ring.author;
    let _ = account.ctx.quiet_line(&ring.conversation, &line, true);
}

/// Checks whether `aci` is a known member of `conversation` (`g.<hex>`).
fn is_group_member(live: &Live, conversation: &str, aci: &str) -> bool {
    if let Some(members) = live.groups.get(conversation) {
        if members.iter().any(|m| m == aci) {
            return true;
        }
    }
    if let Some(info) = live.group_info.get(conversation) {
        if info.member_ids.iter().any(|m| m == aci) {
            return true;
        }
    }
    false
}

/// Pushes current Signal group-call occupants to `ctx.occupants` (driving the live Join banner)
/// and updates `ctx.participants` when we are joined in this group call.
fn publish_group_call_state(account: &Account, conversation: &str) {
    let (occupants, total, joined_participants) = {
        let live = account.live();
        let Some(track) = live.group_calls.get(conversation) else {
            return {
                drop(live);
                account.ctx.occupants(conversation, Vec::new(), 0);
            };
        };
        let occupants: Vec<Occupant> = track
            .members
            .iter()
            .map(|aci| {
                let st = track.states.get(aci).cloned().unwrap_or_default();
                Occupant {
                    person: account.ctx.key(aci),
                    name: if aci == &account.me { account.name.clone() } else { live.name(aci) },
                    avatar: None,
                    muted: st.muted,
                    deafened: false,
                    server_muted: false,
                    server_deafened: false,
                    suppressed: false,
                    streaming: st.sharing,
                    camera: st.camera,
                    speaking: false,
                }
            })
            .collect();
        let total = occupants.len() as u32;
        let joined_participants = live
            .active_group_call
            .as_ref()
            .filter(|c| c.conversation == conversation)
            .map(|_| {
                track
                    .members
                    .iter()
                    .filter(|aci| aci.as_str() != account.me)
                    .map(|aci| {
                        let st = track.states.get(aci).cloned().unwrap_or_default();
                        Participant {
                            id: aci.clone(),
                            name: live.name(aci),
                            avatar: None,
                            muted: st.muted,
                            camera: st.camera,
                            sharing: st.sharing,
                            hand_raised: st.hand_raised,
                            reaction: st.reaction,
                            ..Participant::default()
                        }
                    })
                    .collect::<Vec<_>>()
            });
        (occupants, total, joined_participants)
    };
    account.ctx.occupants(conversation, occupants, total);
    if let Some(participants) = joined_participants {
        account.ctx.participants(conversation, participants);
    }
}

/// Handles `DataMessage.groupCallUpdate` (`era_id` = `Some` when running, `None` when ended).
fn group_call_update(account: &Account, peer: &Peer, by: &str, era_id: Option<String>, sent: u64) {
    let Peer::Group(group_id) = peer else {
        return;
    };
    let conversation = peer.conversation();
    if account.live().blocked.contains(&conversation) {
        return;
    }
    match era_id.filter(|id| !id.is_empty()) {
        Some(era) => {
            let peeked = account.wire.peek_group_call(group_id).ok().filter(|i| !i.members.is_empty());
            let new_era = {
                let mut live = account.live();
                let entry = live.group_calls.entry(conversation.clone()).or_default();
                let is_new = entry.era_id != era;
                entry.era_id = era.clone();
                if let Some(info) = peeked {
                    entry.members = info.members;
                    entry.full = info.full;
                } else if !by.is_empty() && !entry.members.iter().any(|m| m == by) {
                    entry.members.push(by.to_owned());
                }
                is_new
            };
            publish_group_call_state(account, &conversation);
            if new_era && !by.is_empty() {
                let starter = if by == account.me { account.name.clone() } else { account.live().name(by) };
                let mut line = Message::plain(
                    format!("signal-gcall-{era}"),
                    starter.clone(),
                    format!("{starter} started a group call"),
                    i64::try_from(sent / 1_000).unwrap_or(0),
                    by == account.me,
                );
                line.kind = MessageKind::System;
                line.sender.id = by.to_owned();
                let _ = account.ctx.quiet_line(&conversation, &line, false);
            }
        }
        None => {
            account.live().group_calls.remove(&conversation);
            let ring_cleared = {
                let mut live = account.live();
                if live.group_ring.as_ref().is_some_and(|r| r.conversation == conversation) {
                    live.group_ring.take().is_some()
                } else {
                    false
                }
            };
            if ring_cleared {
                account.ctx.ring_stopped(&conversation);
            }
            account.ctx.occupants(&conversation, Vec::new(), 0);
        }
    }
}

/// Applies an SFU roster snapshot (`Event::GroupCallRoster` or peek result).
fn apply_group_call_roster(account: &Account, group_id: &str, info: GroupCallInfo) {
    let conversation = Peer::Group(group_id.to_owned()).conversation();
    if info.members.is_empty() && info.era_id.is_empty() {
        account.live().group_calls.remove(&conversation);
        account.ctx.occupants(&conversation, Vec::new(), 0);
        return;
    }
    {
        let mut live = account.live();
        let entry = live.group_calls.entry(conversation.clone()).or_default();
        if !info.era_id.is_empty() {
            entry.era_id = info.era_id;
        }
        entry.members = info.members;
        entry.full = info.full;
    }
    publish_group_call_state(account, &conversation);
}

/// Applies a remote participant's raise-hand, reaction, or media-state update in a group call.
#[allow(clippy::too_many_arguments)]
fn group_call_participant_state(
    account: &Account,
    group_id: &str,
    aci: &str,
    muted: Option<bool>,
    camera: Option<bool>,
    sharing: Option<bool>,
    hand_raised: Option<bool>,
    reaction: Option<Option<String>>,
) {
    let conversation = Peer::Group(group_id.to_owned()).conversation();
    {
        let mut live = account.live();
        let entry = live.group_calls.entry(conversation.clone()).or_default();
        if !entry.members.iter().any(|m| m == aci) {
            entry.members.push(aci.to_owned());
        }
        let st = entry.states.entry(aci.to_owned()).or_default();
        if let Some(m) = muted {
            st.muted = m;
        }
        if let Some(c) = camera {
            st.camera = c;
        }
        if let Some(s) = sharing {
            st.sharing = s;
        }
        if let Some(h) = hand_raised {
            st.hand_raised = h;
        }
        if let Some(r) = reaction {
            st.reaction = r;
        }
    }
    publish_group_call_state(account, &conversation);
}

/// Decodes incoming `CallMessage.opaque` payloads for Signal group calls (ring, sender key,
/// raise hand, reactions, media state), enforcing group membership before accepting rings or keys.
fn call_opaque_received(account: &Account, _peer: &Peer, by: &str, data: &[u8], _urgent: bool) {
    let Some(signal) = decode_group_call_signal(data) else {
        return;
    };
    let group_id = match &signal {
        GroupCallSignal::Ring { group_id, .. }
        | GroupCallSignal::RaiseHand { group_id, .. }
        | GroupCallSignal::Reaction { group_id, .. }
        | GroupCallSignal::SenderKey { group_id, .. }
        | GroupCallSignal::MediaState { group_id, .. } => group_id.clone(),
    };
    let conversation = Peer::Group(group_id.clone()).conversation();
    // Enforce group membership: reject sender keys, rings, and state updates from non-members.
    if by != account.me && !is_group_member(&account.live(), &conversation, by) {
        tracing::warn!(by, group = %conversation, "chatd: Signal: ignoring group call signal from non-member");
        return;
    }
    match signal {
        GroupCallSignal::Ring { group_id, era_id, ring_id, mode } => match mode {
            GroupRingMode::Ringing => {
                if by == account.me {
                    return;
                }
                let caller = account.live().name(by);
                {
                    let mut live = account.live();
                    live.group_ring = Some(IncomingGroupRing {
                        conversation: conversation.clone(),
                        group_id,
                        era_id: era_id.clone(),
                        caller: by.to_owned(),
                        ring_id,
                    });
                    let entry = live.group_calls.entry(conversation.clone()).or_default();
                    entry.era_id = era_id;
                    if !entry.members.iter().any(|m| m == by) {
                        entry.members.push(by.to_owned());
                    }
                }
                publish_group_call_state(account, &conversation);
                account.ctx.ring(&conversation, &caller);
            }
            GroupRingMode::Cancelled => {
                let stop = {
                    let mut live = account.live();
                    if live.group_ring.as_ref().is_some_and(|r| r.conversation == conversation) {
                        live.group_ring.take().is_some()
                    } else {
                        false
                    }
                };
                if stop {
                    account.ctx.ring_stopped(&conversation);
                }
            }
            GroupRingMode::Declined | GroupRingMode::Busy => {
                let state = if mode == GroupRingMode::Declined { "declined" } else { "busy" };
                let updated = {
                    let mut live = account.live();
                    live.active_group_call
                        .as_mut()
                        .filter(|c| c.conversation == conversation)
                        .map(|c| {
                            if let Some(r) = c.ringing.iter_mut().find(|r| r.id == by) {
                                r.state = state.to_owned();
                            } else {
                                c.ringing.push(CallRing { id: by.to_owned(), name: String::new(), state: state.to_owned() });
                            }
                            c.ringing.clone()
                        })
                };
                if let Some(mut ringing) = updated {
                    for r in &mut ringing {
                        if r.name.is_empty() {
                            r.name = account.live().name(&r.id);
                        }
                    }
                    account.ctx.set_ringing(&conversation, ringing);
                }
            }
        },
        GroupCallSignal::SenderKey { group_id: _, era_id, key_bytes } => {
            if key_bytes.len() < 16 {
                return;
            }
            let mut live = account.live();
            let entry = live.group_calls.entry(conversation).or_default();
            if entry.era_id.is_empty() || entry.era_id == era_id {
                entry.era_id = era_id;
                entry.sender_keys.insert(by.to_owned(), key_bytes);
            }
        }
        GroupCallSignal::RaiseHand { group_id, era_id: _, raised } => {
            group_call_participant_state(account, &group_id, by, None, None, None, Some(raised), None);
        }
        GroupCallSignal::Reaction { group_id, era_id: _, emoji } => {
            let val = (!emoji.is_empty()).then_some(emoji);
            group_call_participant_state(account, &group_id, by, None, None, None, None, Some(val));
        }
        GroupCallSignal::MediaState { group_id, era_id: _, muted, camera, sharing } => {
            group_call_participant_state(
                account,
                &group_id,
                by,
                Some(muted),
                Some(camera),
                Some(sharing),
                None,
                None,
            );
        }
    }
}

/// Joins an active Signal group call or starts a new one through Signal's SFU + `mediad`.
fn group_call_join(account: &Account, conversation_id: &str, group_id: &str) {
    let (is_member, group_members) = {
        let live = account.live();
        let members = live
            .groups
            .get(conversation_id)
            .cloned()
            .or_else(|| live.group_info.get(conversation_id).map(|g| g.member_ids.clone()))
            .unwrap_or_default();
        let member = members.is_empty() || members.iter().any(|m| m == &account.me);
        (member, members)
    };
    if !is_member {
        account.ctx.call_failed(conversation_id, CallFailure::Refused, "You are no longer a member of this group.");
        return;
    }

    // Clear any incoming group ring for this conversation.
    let incoming_ring = {
        let mut live = account.live();
        if live.group_ring.as_ref().is_some_and(|r| r.conversation == conversation_id) {
            live.group_ring.take()
        } else {
            None
        }
    };

    let creds = match account.wire.group_call_credentials(group_id) {
        Ok(c) => c,
        Err(fault) => {
            account.ctx.call_failed(conversation_id, CallFailure::Refused, &fault.words());
            return;
        }
    };
    let peeked = account.wire.peek_group_call(group_id).ok();
    let starting_new = peeked.as_ref().is_none_or(|p| p.era_id.is_empty() && p.members.is_empty())
        && incoming_ring.is_none();
    let era_id = incoming_ring
        .as_ref()
        .map(|r| r.era_id.clone())
        .or_else(|| peeked.as_ref().filter(|p| !p.era_id.is_empty()).map(|p| p.era_id.clone()))
        .unwrap_or_else(|| creds.era_id.clone());

    let always_relay = account.ctx.hub.settings().on(protocol::settings::SIGNAL_ALWAYS_RELAY);
    let relays = signal_relays(account);
    let offer_spec = SignalOffer {
        user_id: account.me.clone(),
        peer_id: group_id.to_owned(),
        host: "127.0.0.1".to_owned(),
        video: true,
        relay_only: always_relay,
        relays,
        muted: false,
        deafened: false,
    };
    let offer_sdp = match account.ctx.signal_offer(conversation_id, offer_spec) {
        Ok(sdp) => sdp,
        Err(detail) => {
            account.ctx.call_failed(conversation_id, CallFailure::Engine, &detail);
            return;
        }
    };

    let (answer_sdp, roster) = match account.wire.group_call_join(group_id, &era_id, &offer_sdp) {
        Ok(res) => res,
        Err(fault) => {
            account.ctx.call_failed(conversation_id, CallFailure::Refused, &fault.words());
            return;
        }
    };

    // Distribute frame-encryption sender key ONLY to verified group members via 1:1 `CallMessage.opaque`.
    let mut sender_key = [0u8; 32];
    let _ = getrandom::fill(&mut sender_key);
    let key_opaque = encode_group_call_signal(&GroupCallSignal::SenderKey {
        group_id: group_id.to_owned(),
        era_id: roster.era_id.clone(),
        key_bytes: sender_key.to_vec(),
    });
    for member_aci in &group_members {
        if member_aci != &account.me {
            let _ = account.wire.send(
                &Peer::Direct(member_aci.clone()),
                Body::Call(CallSignal::Opaque { data: key_opaque.clone(), urgent: true }),
            );
        }
    }

    // If starting a fresh group call, notify the group via `DataMessage.groupCallUpdate`.
    if starting_new {
        let _ = account.wire.send(
            &Peer::Group(group_id.to_owned()),
            Body::GroupCallUpdate { era_id: Some(roster.era_id.clone()) },
        );
    }

    // Update local active group call & roster state.
    {
        let mut live = account.live();
        live.active_group_call = Some(ActiveGroupCall {
            conversation: conversation_id.to_owned(),
            group_id: group_id.to_owned(),
            era_id: roster.era_id.clone(),
            muted: false,
            deafened: false,
            camera: false,
            sharing: false,
            hand_raised: false,
            reaction: None,
            ringing: Vec::new(),
        });
        let entry = live.group_calls.entry(conversation_id.to_owned()).or_default();
        entry.era_id = roster.era_id.clone();
        entry.members = roster.members.clone();
        entry.full = roster.full;
        entry.sender_keys.insert(account.me.clone(), sender_key.to_vec());
    }

    // Build E2EE security badge for the Signal group call.
    let sec_members: Vec<SecureMember> = roster
        .members
        .iter()
        .filter(|m| m.as_str() != account.me)
        .map(|aci| SecureMember {
            user_id: aci.clone(),
            key: String::new(),
            safety: String::new(),
            trust: match account.live().identity_states.get(aci) {
                Some(wire::IdentityState::Verified) => Trust::Verified,
                _ => Trust::Unverified,
            },
        })
        .collect();
    account.ctx.call_security(
        conversation_id,
        Some(CallSecurity {
            mode: SecurityMode::EndToEnd,
            code: String::new(),
            emojis: Vec::new(),
            epoch: 1,
            members: sec_members,
            unsupported: Vec::new(),
            server_lacks: false,
            required: true,
            identity: String::new(),
            stream: false,
        }),
    );
    publish_group_call_state(account, conversation_id);

    if answer_sdp.starts_with("v=0") {
        if let Err(detail) = account.ctx.signal_answer(conversation_id, answer_sdp) {
            account.ctx.call_failed(conversation_id, CallFailure::Engine, &detail);
        }
    } else {
        account.ctx.call_state(conversation_id, CallState::Joined);
    }
}

/// Leaves or declines a Signal group call (`Command::CallLeave`).
fn group_call_leave(account: &Account, conversation_id: &str) {
    let declined_ring = {
        let mut live = account.live();
        if live.group_ring.as_ref().is_some_and(|r| r.conversation == conversation_id) {
            live.group_ring.take()
        } else {
            None
        }
    };
    if let Some(ring) = declined_ring {
        account.ctx.ring_stopped(&ring.conversation);
        let data = encode_group_call_signal(&GroupCallSignal::Ring {
            group_id: ring.group_id,
            era_id: ring.era_id,
            ring_id: ring.ring_id,
            mode: GroupRingMode::Declined,
        });
        let _ = account.wire.send(
            &Peer::Direct(ring.caller),
            Body::Call(CallSignal::Opaque { data, urgent: true }),
        );
        return;
    }

    let ended = {
        let mut live = account.live();
        if live.active_group_call.as_ref().is_some_and(|c| c.conversation == conversation_id) {
            live.active_group_call.take()
        } else {
            None
        }
    };
    let Some(call) = ended else {
        return;
    };
    let _ = account.wire.group_call_leave(&call.group_id, &call.era_id);
    let was_last = {
        let mut live = account.live();
        if let Some(track) = live.group_calls.get_mut(conversation_id) {
            track.members.retain(|m| m != &account.me);
            track.states.remove(&account.me);
            if track.members.is_empty() {
                live.group_calls.remove(conversation_id);
                true
            } else {
                false
            }
        } else {
            true
        }
    };
    if was_last {
        let _ = account.wire.send(&Peer::Group(call.group_id), Body::GroupCallUpdate { era_id: None });
    }
    publish_group_call_state(account, conversation_id);
}

/// Rings or stops ringing members of the active Signal group call (`Command::CallRing` / `CallStopRinging`).
fn group_call_ring(account: &Account, conversation_id: &str, people: Option<Vec<String>>, stop: bool) -> Result<(), Refusal> {
    let (group_id, era_id, targets) = {
        let live = account.live();
        let Some(call) = live.active_group_call.as_ref().filter(|c| c.conversation == conversation_id) else {
            return Err(Refusal::Refused("Join the group call before ringing members.".to_owned()));
        };
        let all_members = live
            .groups
            .get(conversation_id)
            .cloned()
            .or_else(|| live.group_info.get(conversation_id).map(|g| g.member_ids.clone()))
            .unwrap_or_default();
        let in_call: HashSet<String> = live
            .group_calls
            .get(conversation_id)
            .map(|t| t.members.iter().cloned().collect())
            .unwrap_or_default();
        let wanted: Vec<String> = match people {
            Some(list) => list.into_iter().filter(|id| id != &account.me && all_members.iter().any(|m| m == id)).collect(),
            None => all_members.into_iter().filter(|id| id != &account.me && !in_call.contains(id)).collect(),
        };
        (call.group_id.clone(), call.era_id.clone(), wanted)
    };
    let mode = if stop { GroupRingMode::Cancelled } else { GroupRingMode::Ringing };
    let data = encode_group_call_signal(&GroupCallSignal::Ring {
        group_id,
        era_id,
        ring_id: now_ms() as i64,
        mode,
    });
    let mut ringing_list = Vec::new();
    for target in &targets {
        let _ = account.wire.send(
            &Peer::Direct(target.clone()),
            Body::Call(CallSignal::Opaque { data: data.clone(), urgent: true }),
        );
        if !stop {
            ringing_list.push(CallRing {
                id: target.clone(),
                name: account.live().name(target),
                state: "ringing".to_owned(),
            });
        }
    }
    if let Some(active) = account.live().active_group_call.as_mut().filter(|c| c.conversation == conversation_id) {
        active.ringing = ringing_list.clone();
    }
    account.ctx.set_ringing(conversation_id, ringing_list);
    Ok(())
}

/// Toggles raising/lowering our hand in the active Signal group call (`Command::CallRaiseHand`).
fn group_call_raise_hand(account: &Account, conversation_id: &str, raised: bool) -> Result<(), Refusal> {
    let (group_id, era_id) = {
        let mut live = account.live();
        let Some(active) = live.active_group_call.as_mut().filter(|c| c.conversation == conversation_id) else {
            return Err(Refusal::Refused("Raise hand is only available in a Signal group call.".to_owned()));
        };
        active.hand_raised = raised;
        (active.group_id.clone(), active.era_id.clone())
    };
    account
        .wire
        .group_call_send_data(&group_id, GroupCallSignal::RaiseHand { group_id: group_id.clone(), era_id, raised })
        .map_err(refused)?;
    account.ctx.call_hand_raised(conversation_id, raised);
    Ok(())
}

/// Sends a transient emoji reaction in the active Signal group call (`Command::CallReact`).
fn group_call_react(account: &Account, conversation_id: &str, emoji: String) -> Result<(), Refusal> {
    let emoji = emoji.trim().to_owned();
    let (group_id, era_id) = {
        let mut live = account.live();
        let Some(active) = live.active_group_call.as_mut().filter(|c| c.conversation == conversation_id) else {
            return Err(Refusal::Refused("Call reactions are only available in a Signal group call.".to_owned()));
        };
        active.reaction = (!emoji.is_empty()).then(|| emoji.clone());
        (active.group_id.clone(), active.era_id.clone())
    };
    account
        .wire
        .group_call_send_data(&group_id, GroupCallSignal::Reaction { group_id: group_id.clone(), era_id, emoji: emoji.clone() })
        .map_err(refused)?;
    account.ctx.call_reaction(conversation_id, (!emoji.is_empty()).then_some(emoji));
    Ok(())
}

fn call_event_label(kind: wire::CallKind, incoming: bool, outcome: wire::CallOutcome) -> &'static str {
    match (outcome, kind, incoming) {
        (wire::CallOutcome::Observed, _, _) => "Answered on another device",
        (wire::CallOutcome::Accepted, wire::CallKind::Voice, true) => "Incoming voice call",
        (wire::CallOutcome::Accepted, wire::CallKind::Video, true) => "Incoming video call",
        (wire::CallOutcome::Accepted, wire::CallKind::Voice, false) => "Outgoing voice call",
        (wire::CallOutcome::Accepted, wire::CallKind::Video, false) => "Outgoing video call",
        (wire::CallOutcome::Accepted, wire::CallKind::Group, _) => "Group call",
        (wire::CallOutcome::Accepted, wire::CallKind::AdHoc, _) => "Call link",
        (wire::CallOutcome::NotAccepted, wire::CallKind::Voice, true) => "Missed voice call",
        (wire::CallOutcome::NotAccepted, wire::CallKind::Video, true) => "Missed video call",
        (wire::CallOutcome::NotAccepted, wire::CallKind::Voice, false) => "Cancelled voice call",
        (wire::CallOutcome::NotAccepted, wire::CallKind::Video, false) => "Cancelled video call",
        (wire::CallOutcome::NotAccepted, wire::CallKind::Group, _) => "Missed group call",
        (wire::CallOutcome::NotAccepted, wire::CallKind::AdHoc, _) => "Missed call",
        (wire::CallOutcome::Deleted, _, _) => "",
    }
}

fn apply_call_event(
    account: &Account,
    peer: &Peer,
    call_id: u64,
    timestamp: u64,
    kind: wire::CallKind,
    incoming: bool,
    outcome: wire::CallOutcome,
) {
    let conversation = peer.conversation();
    let msg_id = format!("signal-call-{call_id}");
    if outcome == wire::CallOutcome::Deleted {
        account.live().call_history.retain(|(c, id)| !(c == &conversation && *id == call_id));
        let _ = account.ctx.deleted(&conversation, &msg_id);
        return;
    }
    // If an incoming ring is active for this call/peer and it was accepted/observed elsewhere, stop ringing.
    if matches!(outcome, wire::CallOutcome::Accepted | wire::CallOutcome::Observed) {
        let stop = {
            let mut live = account.live();
            if live.ring.as_ref().is_some_and(|r| r.id == call_id || r.conversation == conversation) {
                live.ring.take()
            } else {
                None
            }
        };
        if let Some(ring) = stop {
            account.ctx.ring_stopped(&ring.conversation);
        }
    }
    match peer {
        Peer::Direct(aci) => {
            let title = account.live().names.get(aci).cloned().unwrap_or_default();
            ensure_row(account, peer, &title);
        }
        Peer::Group(_) => {
            if let Ok(None) = account.ctx.known(&conversation) {
                let _ = account.ctx.conversation(&row(peer, ""));
            }
        }
    }
    let label = call_event_label(kind, incoming, outcome);
    let author_id = if incoming {
        match peer {
            Peer::Direct(aci) => aci.clone(),
            Peer::Group(_) => String::new(),
        }
    } else {
        account.me.clone()
    };
    let author_name = if author_id == account.me {
        account.name.clone()
    } else if author_id.is_empty() {
        String::new()
    } else {
        account.live().name(&author_id)
    };
    let mut line = Message::plain(msg_id, author_name, label, i64::try_from(timestamp / 1_000).unwrap_or(0), !incoming);
    line.kind = MessageKind::System;
    line.sender.id = author_id;
    {
        let mut live = account.live();
        if !live.call_history.iter().any(|(c, id)| c == &conversation && *id == call_id) {
            if live.call_history.len() >= 500 {
                live.call_history.pop_front();
            }
            live.call_history.push_back((conversation.clone(), call_id));
        }
    }
    let unread = incoming && outcome == wire::CallOutcome::NotAccepted;
    let _ = account.ctx.quiet_line(&conversation, &line, unread);
}

fn apply_call_log_event(account: &Account, kind: wire::CallLogKind, peer: Option<&Peer>, _call_id: Option<u64>, _timestamp: u64) {
    match kind {
        wire::CallLogKind::Clear => {
            let entries: Vec<(String, u64)> = account.live().call_history.drain(..).collect();
            for (conv, cid) in entries {
                let _ = account.ctx.deleted(&conv, &format!("signal-call-{cid}"));
            }
        }
        wire::CallLogKind::ClearInConversation => {
            let Some(conv) = peer.map(Peer::conversation) else {
                return;
            };
            let mut removed = Vec::new();
            account.live().call_history.retain(|(c, cid)| {
                if c == &conv {
                    removed.push(*cid);
                    false
                } else {
                    true
                }
            });
            for cid in removed {
                let _ = account.ctx.deleted(&conv, &format!("signal-call-{cid}"));
            }
        }
        wire::CallLogKind::MarkedAsRead => {
            let mut latest: HashMap<String, u64> = HashMap::new();
            for (conv, cid) in account.live().call_history.iter() {
                latest.insert(conv.clone(), *cid);
            }
            for (conv, cid) in latest {
                let updated = account.ctx.known(&conv).ok().flatten().map_or(0, |row| row.updated);
                let _ = account.ctx.counts(&conv, 0, 0, Some(&format!("signal-call-{cid}")), updated);
            }
        }
        wire::CallLogKind::MarkedAsReadInConversation => {
            if let Some(conv) = peer.map(Peer::conversation) {
                let last_cid = account
                    .live()
                    .call_history
                    .iter()
                    .rev()
                    .find(|(c, _)| c == &conv)
                    .map(|(_, cid)| format!("signal-call-{cid}"));
                let updated = account.ctx.known(&conv).ok().flatten().map_or(0, |row| row.updated);
                let _ = account.ctx.counts(&conv, 0, 0, last_cid.as_deref(), updated);
            }
        }
    }
}

/// A Signal username as libsignal's `Username::new` accepts one: `nickname.42`, optionally with
/// one leading `@`. A plain name, a phone number, and a `signal.me` link are not usernames, so
/// they are not sent to Signal.
fn username_of(query: &str) -> Option<&str> {
    let username = query.trim().strip_prefix('@').unwrap_or(query.trim());
    let (nickname, discriminator) = username.rsplit_once('.')?;
    (nickname_ok(nickname) && discriminator_ok(discriminator)).then_some(username)
}

/// Nickname: 1 to 48 of `[_0-9A-Za-z]`, and not starting with a digit. libsignal's hard limit.
fn nickname_ok(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_digit() || !nickname_char(first) {
        return false;
    }
    let mut count = 1usize;
    for ch in chars {
        if !nickname_char(ch) {
            return false;
        }
        count += 1;
        if count > 48 {
            return false;
        }
    }
    true
}

fn nickname_char(ch: char) -> bool {
    matches!(ch, '_' | 'a'..='z' | 'A'..='Z' | '0'..='9')
}

/// Discriminator: at least two digits, not zero, and no leading zero once it is longer than two.
/// `01` is a username; `1` and `042` are not.
fn discriminator_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() == 1 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if bytes.len() > 2 && bytes[0] == b'0' {
        return false;
    }
    text.parse::<u64>().ok().is_some_and(|value| value != 0)
}

/// Looks up a username and stores the account it names. Anything else only finishes the search.
fn search_people(account: &Account, query: &str) {
    if let Some(username) = username_of(query) {
        match account.wire.lookup(username) {
            Ok(Some(aci)) if wire::is_aci(&aci) => {
                let name = {
                    let mut live = account.live();
                    live.usernames.insert(aci.clone(), username.to_owned());
                    let known = live.known_name(&aci);
                    if known.is_empty() {
                        live.names.insert(aci.clone(), username.to_owned());
                        username.to_owned()
                    } else {
                        known
                    }
                };
                let _ = account.ctx.people(&[PersonRow {
                    avatar: account.ctx.avatar_of(&aci),
                    id: aci,
                    name,
                    handle: username.to_owned(),
                    ..PersonRow::default()
                }]);
            }
            Ok(_) => {}
            Err(fault) => tracing::warn!("chatd: Signal: username: {}", super::net::redact(&fault.to_string())),
        }
    }
    account.ctx.searched(query);
}

/// The lab's signal-wave19 driver: proves Signal 1:1 voice/video calling (#716) and Signal
/// group calls (#719) capabilities, relay policy, wire encoding/decoding, and parity table.
pub fn prove_signal_wave19() -> Result<(), String> {
    use protocol::{CallOffer, CallSupport, MediaRequest, MediaResponse, Network, Participant, SignalAccept, SignalOffer, SignalRelay};

    if call_offer(ConversationKind::Direct) != CallOffer::Native
        || call_offer(ConversationKind::Direct).support() != CallSupport::Native
    {
        return Err("Signal direct calls must advertise CallSupport::Native".to_owned());
    }
    if call_offer(ConversationKind::Group) != CallOffer::Native
        || call_offer(ConversationKind::Group).support() != CallSupport::Native
    {
        return Err("Signal group calls must advertise CallSupport::Native".to_owned());
    }
    if !protocol::call_pictures(Network::Signal) {
        return Err("protocol::call_pictures(Network::Signal) must be true".to_owned());
    }
    if !protocol::settings::signal_relay_only(false, false)
        || !protocol::settings::signal_relay_only(true, true)
        || protocol::settings::signal_relay_only(true, false)
    {
        return Err("signal_relay_only policy check failed".to_owned());
    }
    let sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n";
    let opaque = wire::encode_sdp_opaque(sdp, &[]);
    if wire::decode_sdp_opaque(&opaque) != sdp {
        return Err("SDP opaque round-trip failed".to_owned());
    }
    for sig in [
        GroupCallSignal::Ring {
            group_id: "g1".into(),
            era_id: "era-1".into(),
            ring_id: 7,
            mode: GroupRingMode::Ringing,
        },
        GroupCallSignal::RaiseHand { group_id: "g1".into(), era_id: "era-1".into(), raised: true },
        GroupCallSignal::Reaction { group_id: "g1".into(), era_id: "era-1".into(), emoji: "👏".into() },
        GroupCallSignal::SenderKey { group_id: "g1".into(), era_id: "era-1".into(), key_bytes: vec![0x42; 32] },
        GroupCallSignal::MediaState {
            group_id: "g1".into(),
            era_id: "era-1".into(),
            muted: false,
            camera: true,
            sharing: true,
        },
    ] {
        let enc = encode_group_call_signal(&sig);
        if decode_group_call_signal(&enc) != Some(sig) {
            return Err("GroupCallSignal opaque round-trip failed".to_owned());
        }
    }
    let offer_req = MediaRequest::SignalOffer(SignalOffer {
        user_id: "me".into(),
        peer_id: "grace".into(),
        host: "127.0.0.1".into(),
        video: true,
        relay_only: true,
        relays: vec![SignalRelay {
            urls: vec!["turn:127.0.0.1:3478?transport=udp".into()],
            username: "u".into(),
            password: "p".into(),
        }],
        muted: false,
        deafened: false,
    });
    let json = serde_json::to_string(&offer_req).map_err(|e| e.to_string())?;
    if serde_json::from_str::<MediaRequest>(&json).map_err(|e| e.to_string())? != offer_req {
        return Err("SignalOffer MediaRequest round-trip failed".to_owned());
    }
    let accept_req = MediaRequest::SignalAccept(SignalAccept {
        user_id: "me".into(),
        peer_id: "grace".into(),
        host: "127.0.0.1".into(),
        offer_sdp: sdp.to_owned(),
        video: true,
        relay_only: false,
        relays: Vec::new(),
        muted: false,
        deafened: false,
    });
    let json = serde_json::to_string(&accept_req).map_err(|e| e.to_string())?;
    if serde_json::from_str::<MediaRequest>(&json).map_err(|e| e.to_string())? != accept_req {
        return Err("SignalAccept MediaRequest round-trip failed".to_owned());
    }
    let ans = MediaResponse::Answer { sdp: sdp.to_owned() };
    if !ans.is_reply() || ans.name() != "Answer" {
        return Err("MediaResponse::Answer contract check failed".to_owned());
    }
    let participant = Participant {
        id: "grace".into(),
        name: "Grace".into(),
        hand_raised: true,
        reaction: Some("🎉".into()),
        sharing: true,
        ..Participant::default()
    };
    let pjson = serde_json::to_string(&participant).map_err(|e| e.to_string())?;
    if serde_json::from_str::<Participant>(&pjson).map_err(|e| e.to_string())? != participant {
        return Err("Participant raise-hand/reaction round-trip failed".to_owned());
    }
    let parity = crate::adapters::parity::markdown();
    for label in ["Direct audio", "Group audio", "Camera", "Screen share", "Ring a direct call"] {
        let row = parity
            .lines()
            .find(|l| l.starts_with(&format!("| {label} |")))
            .ok_or_else(|| format!("missing parity row {label}"))?;
        let cols: Vec<&str> = row.split('|').map(str::trim).filter(|c| !c.is_empty()).collect();
        // Order: label(0), Discord(1), Slack(2), Telegram(3), Teams(4), Signal(5), Messages(6), Tiny(7)
        if cols.get(5).copied() != Some("N") {
            return Err(format!("Signal parity row {label} expected N, got {:?}", cols.get(5)));
        }
    }
    Ok(())
}

/// The lab checks this without a Signal account: which typed queries are usernames.
pub fn prove_signal_username() -> Result<(), String> {
    for username in ["ada.42", "@ada.42", "Ada.42", "@Ada.42", "He110.01", "usr.999999999", "_identifier.42"] {
        if username_of(username).is_none() {
            return Err(format!("{username} should be a username"));
        }
    }
    if username_of("@Ada.42") != Some("Ada.42") {
        return Err("a leading @ is not part of the username".to_owned());
    }
    for query in ["Grace", "+15550100002", "https://signal.me/#eu/abc", "ada.1", "ada.042", "1ada.42", "ada.00"] {
        if username_of(query).is_some() {
            return Err(format!("{query} should not be looked up"));
        }
    }
    Ok(())
}

/// Answers a command while there is no connection.
fn offline(ctx: &Context, command: Command) {
    let down = || Refusal::Refused("Signal is offline.".to_owned());
    match command {
        Command::Send { conversation_id, client_id, .. } | Command::SendWith { conversation_id, client_id, .. } => {
            ctx.sent(&conversation_id, &client_id, Err("Signal is offline.".to_owned()));
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&ctx.account, &client_id),
        Command::React { reply, .. }
        | Command::Edit { reply, .. }
        | Command::Delete { reply, .. }
        | Command::SetMute { reply, .. }
        | Command::File { reply, .. }
        | Command::Relate { reply, .. }
        | Command::AnswerMessageRequest { reply, .. }
        | Command::Report { reply, .. }
        | Command::Leave { reply, .. }
        | Command::MembersAdd { reply, .. }
        | Command::MembersRemove { reply, .. }
        | Command::ConversationUpdate { reply, .. }
        | Command::ConversationPicture { reply, .. } => drop(reply.send(Err(down()))),
        Command::Members { reply, .. } => drop(reply.send(Err(down()))),
        Command::ConversationInfo { reply, .. } => drop(reply.send(Err(down()))),
        Command::Profile { reply, .. } => drop(reply.send(Err(down()))),
        Command::Pinned { reply, .. } => drop(reply.send(Err(down()))),
        Command::Pin { reply, .. } => drop(reply.send(Err(down()))),
        Command::Catalog { reply, .. } => drop(reply.send(Err(down()))),
        Command::StartDirect { reply, .. } | Command::StartGroup { reply, .. } => drop(reply.send(Err(down()))),
        Command::SearchPeople(query) => ctx.searched(&query),
        Command::SearchShared { conversation_id, kind, before, limit, reply } => {
            let key = ctx.key(&conversation_id);
            let res = match ctx.hub.store().shared(&key, kind, before.as_deref(), limit) {
                Ok(Some((items, more))) => Ok(SharedPage { items, more, total: None }),
                Ok(None) => Err(Refusal::Refused("no such conversation".into())),
                Err(err) => Err(Refusal::Refused(err.to_string())),
            };
            let _ = reply.send(res);
        }
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        Command::CallJoin(conversation_id) => {
            ctx.call_failed(&conversation_id, CallFailure::Lost, "Signal is offline.");
        }
        other => other.unsupported(ctx),
    }
}

/// What reactions a Signal conversation takes: any one emoji, and one reaction a person on a
/// message, a new one replacing it (which Signal's own apps do, and its protocol carries one
/// emoji per person). No custom emoji, and no cap on how many different ones a message holds.
pub fn reaction_rules() -> protocol::ReactionRules {
    protocol::ReactionRules { mine: 1, replaces: true, ..protocol::ReactionRules::default() }
}

/// The ceiling: link previews on, which is what a linked account is asked for.
pub(crate) fn capability_ceiling() -> protocol::Capabilities {
    capabilities(true)
}

/// Signal direct 1:1 calls and group calls run natively through `mediad` (pure-Rust `str0m`
/// Opus + H.264). Signal has no voice rooms (`Voice` is absent).
pub(crate) fn call_offer(kind: ConversationKind) -> protocol::CallOffer {
    match kind {
        ConversationKind::Direct | ConversationKind::Group => protocol::CallOffer::Native,
        _ => protocol::CallOffer::Absent,
    }
}

/// What this account may send: text up to the body other clients keep in the message itself,
/// files up to Signal's limits, and a one-day edit window (how long Signal Desktop still sends an
/// edit; a late delete is delete-for-me instead, decided in `delete`).
fn capabilities(link_previews: bool) -> protocol::Capabilities {
    let files = files::capabilities();
    protocol::Capabilities {
        text_limit: TEXT_LIMIT,
        edit_window: (SEND_WINDOW_MS / 1_000) as u32,
        max_file_bytes: files.max_file_bytes,
        max_files: files.max_files,
        voice_notes: true,
        gifs: true,
        stickers: true,
        reply: true,
        edit: true,
        delete: true,
        pins: true,
        forward: false,
        suppress_embeds: link_previews,
        polls: Some(protocol::PollCapabilities {
            max_options: protocol::POLL_OPTIONS as u8,
            max_duration_secs: 0,
            multiple: true,
            anonymous: false,
        }),
        reactions: true,
        mention_format: protocol::MentionFormat::Signal,
        mention_spans: false,
        disappearing: true,
        view_once: false,
        profile_name: true,
        profile_about: true,
        pin_chats: 4,
        archive: true,
        groups: 1_000,
        blocks: true,
        ..protocol::Capabilities::default()
    }
}

fn serve(account: &Arc<Account>, command: Command) {
    let ctx = &account.ctx;
    match command {
        Command::MarkRead { conversation_id, message_id } => {
            mark_read(account, &conversation_id, &message_id);
            if !account.live().folders.is_empty() {
                sync_filing(account, &conversation_id);
            }
        }
        Command::MarkUnread { conversation_id, .. } => {
            if let Ok(to) = peer(&conversation_id) {
                let _ = account.wire.set_marked_unread(&to, true);
            }
            let _ = ctx.mark_unread(&conversation_id);
            if !account.live().folders.is_empty() {
                sync_filing(account, &conversation_id);
            }
        }
        Command::Viewed { conversation_id, message_id } => {
            viewed(account, &conversation_id, &message_id);
        }
        Command::SetMute { conversation_id, notify, until, reply } => {
            let outcome = (|| -> Result<(), Refusal> {
                let to = peer(&conversation_id).map_err(refused)?;
                let (muted, until_ms) = match notify {
                    protocol::Notify::Nothing => {
                        let ms = until
                            .filter(|&s| s > 0)
                            .map(|s| (s as u64).saturating_mul(1_000))
                            .unwrap_or(i64::MAX as u64);
                        (true, ms)
                    }
                    protocol::Notify::All | protocol::Notify::Mentions => (false, 0),
                };
                account.wire.set_mute(&to, until_ms).map_err(refused)?;
                {
                    let mut live = account.live();
                    if muted {
                        live.muted_until.insert(conversation_id.clone(), until_ms);
                    } else {
                        live.muted_until.remove(&conversation_id);
                    }
                }
                let _ = ctx.muted(&conversation_id, muted);
                if !account.live().folders.is_empty() {
                    sync_filing(account, &conversation_id);
                }
                Ok(())
            })();
            let _ = reply.send(outcome);
        }
        Command::File { conversation_id, pinned, archived, folder_add: _, folder_remove: _, reply } => {
            let outcome = (|| -> Result<(), Refusal> {
                let to = peer(&conversation_id).map_err(refused)?;
                if let Some(arch) = archived {
                    account.wire.set_archived(&to, arch).map_err(refused)?;
                    let mut live = account.live();
                    if arch {
                        live.archived.insert(conversation_id.clone());
                    } else {
                        live.archived.remove(&conversation_id);
                    }
                }
                let mut affected_pins = Vec::new();
                if let Some(pin) = pinned {
                    account.wire.set_pinned(&to, pin).map_err(refused)?;
                    let mut live = account.live();
                    if pin {
                        if !live.pinned.contains(&conversation_id) {
                            if live.pinned.len() >= 4 {
                                return Err(Refusal::Refused("Signal allows at most 4 pinned chats.".to_owned()));
                            }
                            live.pinned.push(conversation_id.clone());
                        }
                    } else {
                        live.pinned.retain(|id| id != &conversation_id);
                    }
                    affected_pins = live.pinned.clone();
                }
                sync_filing(account, &conversation_id);
                for id in affected_pins {
                    if id != conversation_id {
                        sync_filing(account, &id);
                    }
                }
                Ok(())
            })();
            let _ = reply.send(outcome);
        }
        Command::Send { conversation_id, client_id, text, reply_to } => {
            let outcome = send_text(account, &conversation_id, &text, reply_to.as_deref(), &[], false);
            ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
        }
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            if let Some(draft) = with.poll.clone() {
                if !with.files.is_empty() {
                    ctx.sent(&conversation_id, &client_id, Err("A poll can't carry a file.".to_owned()));
                } else {
                    let outcome = send_poll(account, &conversation_id, &draft, reply_to.as_deref());
                    ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
                }
            } else if let Some(contact) = with.contact.clone() {
                let outcome = send_contact(account, &conversation_id, contact);
                ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
            } else {
                send_with(account, &conversation_id, &client_id, &text, reply_to.as_deref(), *with);
            }
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&account.ctx.account, &client_id),
        Command::React { conversation_id, message_id, emoji, on, burst: _, reply } => {
            let _ = reply.send(react(account, &conversation_id, &message_id, &emoji, on).map_err(refused));
        }
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        Command::Edit { conversation_id, message_id, text, spans, suppress_embeds: _, keep_attachments: _, reply } => {
            let answered = match poll_action(&text) {
                Some(act) => poll_act(account, &conversation_id, &message_id, act),
                None => edit(account, &conversation_id, &message_id, &text, &spans),
            };
            let _ = reply.send(answered.map_err(refused));
        }
        Command::Delete { conversation_id, message_id, scope, reply } => {
            let answer = match scope {
                protocol::DeleteScope::Everyone => delete(account, &conversation_id, &message_id).map_err(refused),
                protocol::DeleteScope::Me => {
                    let _ = account.ctx.deleted(&conversation_id, &message_id);
                    Ok(())
                }
            };
            let _ = reply.send(answer);
        }
        Command::Vote { conversation_id, message_id, votes, reply } => {
            let act = match votes {
                Some(indexes) => PollAct::Vote(indexes),
                None => PollAct::End,
            };
            let _ = reply.send(poll_act(account, &conversation_id, &message_id, act).map_err(refused));
        }
        Command::Typing { conversation_id, activity: _ } => typing(account, &conversation_id),
        Command::StartDirect { person_id, reply } => {
            if let Some(url) = person_id.strip_prefix(GROUP_JOIN_LINK) {
                let res = account
                    .wire
                    .group_join_by_link(url.trim())
                    .map(|g| {
                        let peer = Peer::Group(g.id.clone());
                        let mut conv = row(&peer, &g.title);
                        conv.topic = g.description.chars().take(300).collect();
                        let _ = ctx.conversation(&conv);
                        peer.conversation()
                    })
                    .map_err(refused);
                let _ = reply.send(res);
                return;
            }
            // Nothing reaches the other person: the chat exists on Signal once a message does.
            let started = wire::is_aci(&person_id).then(|| {
                let name = account.live().names.get(&person_id).cloned().unwrap_or_default();
                ensure_row(account, &Peer::Direct(person_id.clone()), &name);
                person_id
            });
            let _ = reply.send(started.ok_or_else(|| Refusal::Refused("That person can't be messaged from here.".to_owned())));
        }
        Command::StartGroup { people, title, reply } => {
            let outcome = (|| -> Result<String, Refusal> {
                let mut members: Vec<String> = Vec::new();
                for id in people {
                    if id == account.me {
                        continue;
                    }
                    if !wire::is_aci(&id) {
                        return Err(Refusal::Refused("Every group member must be a Signal contact.".to_owned()));
                    }
                    if !members.contains(&id) {
                        members.push(id);
                    }
                }
                if members.is_empty() {
                    return Err(Refusal::Refused("Pick at least one person to start a Signal group.".to_owned()));
                }
                let title = match title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
                    Some(t) => t,
                    None => {
                        let live = account.live();
                        let names: Vec<String> = members.iter().take(3).map(|id| live.name(id)).collect();
                        names.join(", ")
                    }
                };
                let created = account.wire.create_group(&title, &members).map_err(refused)?;
                let peer = Peer::Group(created.id.clone());
                let conv = peer.conversation();
                let rights = group_rights(&created, &account.me);
                {
                    let mut live = account.live();
                    live.groups.insert(conv.clone(), created.member_ids.clone());
                    live.group_admins.insert(conv.clone(), created.admins.clone());
                    live.group_info.insert(conv.clone(), created.clone());
                    live.known_conversations.insert(conv.clone());
                    live.accepted.insert(conv.clone());
                }
                let mut group_row = row(&peer, &created.title);
                group_row.topic = created.description.chars().take(300).collect();
                group_row.read_only = !created.member;
                group_row.rights = rights;
                group_row.avatar = ctx.avatar_of(&group_row.id);
                let _ = ctx.conversation(&group_row);
                if !account.live().folders.is_empty() {
                    sync_filing(account, &conv);
                }
                Ok(conv)
            })();
            let _ = reply.send(outcome);
        }
        Command::SearchPeople(query) => search_people(account, &query),
        Command::Members { conversation_id, offset, limit, reply } => {
            let _ = reply.send(group_members(account, &conversation_id, offset, limit));
        }
        Command::ConversationInfo { conversation_id, reply } => {
            let _ = reply.send(conversation_info(account, &conversation_id));
        }
        Command::MembersAdd { conversation_id, people, reply, .. } => {
            let _ = reply.send(members_add(account, &conversation_id, &people));
        }
        Command::MembersRemove { conversation_id, person, reply } => {
            let _ = reply.send(members_remove(account, &conversation_id, &person));
        }
        Command::ConversationUpdate { conversation_id, title, topic, description, reply } => {
            let _ = reply.send(group_update(account, &conversation_id, title, topic.or(description)));
        }
        Command::ConversationPicture { conversation_id, path, reply } => {
            let _ = reply.send(group_picture(account, &conversation_id, &path));
        }
        Command::Pinned { conversation_id, reply } => {
            let _ = reply.send(pinned_messages(account, &conversation_id));
        }
        Command::Pin { conversation_id, message_id, pinned, reply, .. } => {
            let _ = reply.send(pin_message(account, &conversation_id, &message_id, pinned));
        }
        Command::Catalog { kind, query, cursor, reply, .. } => {
            let _ = reply.send(catalog(account, kind, &query, cursor.as_deref()));
        }
        Command::Handoff { conversation_id, reply } => {
            let _ = reply.send(call_link_handoff(account, &conversation_id));
        }
        Command::Wanted(ids) => wanted(account, &ids),
        Command::SearchShared { conversation_id, kind, before, limit, reply } => {
            let _ = reply.send(shared(account, &conversation_id, kind, before.as_deref(), limit));
        }
        Command::Watch(watching) => pictures(account, &watching),
        Command::SignOut(reply) => {
            if let Err(fault) = account.wire.unlink() {
                tracing::warn!("chatd: Signal: unlinking: {}", super::net::redact(&fault.to_string()));
            }
            let _ = reply.send(Ok(()));
        }
        Command::Profile { person_id, reply } => {
            let outcome = if person_id == account.me {
                own_about(account)
            } else {
                person_about(account, &person_id)
            };
            let _ = reply.send(outcome);
        }
        Command::SetPresence { status, reply, .. } => {
            let Some((name, about, emoji)) = status.as_deref().and_then(profile_edit) else {
                let _ = reply.send(Err(Refusal::Unsupported));
                return;
            };
            let _ = reply.send(set_own(account, name, about, emoji));
        }
        Command::Relate { person_id, action, reply } => {
            let outcome = match action {
                RelateAction::Block | RelateAction::Unblock => {
                    let block = action == RelateAction::Block;
                    if !wire::is_aci(&person_id) {
                        Err(Refusal::Refused("That person isn't one Signal knows.".to_owned()))
                    } else {
                        account.wire.set_blocked(&Peer::Direct(person_id.clone()), block).map_err(refused).map(|()| {
                            let rel = {
                                let mut live = account.live();
                                if block {
                                    live.blocked.insert(person_id.clone());
                                    Relation::Blocked
                                } else {
                                    live.blocked.remove(&person_id);
                                    if live.contacts.contains(&person_id) { Relation::Contact } else { Relation::None }
                                }
                            };
                            let _ = ctx.relation(&person_id, rel);
                        })
                    }
                }
                _ => Err(Refusal::Refused("Signal only supports blocking and unblocking.".to_owned())),
            };
            let _ = reply.send(outcome);
        }
        Command::AnswerMessageRequest { conversation_id, accept, reply } => {
            let outcome = (|| -> Result<(), Refusal> {
                let to = peer(&conversation_id).map_err(refused)?;
                match &to {
                    Peer::Group(group_id) => {
                        account.wire.group_pending_invite(group_id, accept).map_err(refused)?;
                    }
                    Peer::Direct(_) => {
                        let action = if accept { wire::MessageRequestAction::Accept } else { wire::MessageRequestAction::Delete };
                        account.wire.respond_message_request(&to, action).map_err(refused)?;
                        if accept {
                            let title = {
                                let mut live = account.live();
                                live.requests.remove(&conversation_id);
                                live.accepted.insert(conversation_id.clone());
                                live.names.get(&conversation_id).cloned().unwrap_or_default()
                            };
                            ensure_row(account, &to, &title);
                        } else {
                            account.live().requests.remove(&conversation_id);
                            let _ = ctx.forget_conversation(&conversation_id);
                        }
                    }
                }
                Ok(())
            })();
            let _ = reply.send(outcome);
        }
        Command::ReportReasons { reply, .. } => {
            let _ = reply.send(Ok(vec![ReportReason::Spam, ReportReason::Harassment, ReportReason::Other]));
        }
        Command::Report { target, block, reply, .. } => {
            let outcome = (|| -> Result<(), Refusal> {
                let to = match &target {
                    ReportTarget::Message { conversation, .. } | ReportTarget::Conversation(conversation) => {
                        peer(&conversation.id).map_err(refused)?
                    }
                    ReportTarget::Person(person) => {
                        if wire::is_aci(&person.id) {
                            Peer::Direct(person.id.clone())
                        } else {
                            return Err(Refusal::Refused("That person isn't one Signal knows.".to_owned()));
                        }
                    }
                };
                let action = if block { wire::MessageRequestAction::BlockAndSpam } else { wire::MessageRequestAction::Spam };
                account.wire.respond_message_request(&to, action).map_err(refused)?;
                if block {
                    let _ = account.wire.set_blocked(&to, true);
                    let conv = to.conversation();
                    {
                        let mut live = account.live();
                        live.blocked.insert(conv.clone());
                        live.requests.remove(&conv);
                    }
                    if let Peer::Direct(aci) = &to {
                        let _ = ctx.relation(aci, Relation::Blocked);
                    }
                }
                Ok(())
            })();
            let _ = reply.send(outcome);
        }
        Command::Leave { conversation_id, silent: _, reply } => {
            let outcome = (|| -> Result<(), Refusal> {
                let Peer::Group(group_id) = peer(&conversation_id).map_err(refused)? else {
                    return Err(Refusal::Refused("Only a group can be left.".to_owned()));
                };
                {
                    let live = account.live();
                    let admins = live.group_admins.get(&conversation_id).map(Vec::as_slice).unwrap_or(&[]);
                    let members = live.groups.get(&conversation_id).map(Vec::as_slice).unwrap_or(&[]);
                    let me_admin = admins.iter().any(|id| id == &account.me);
                    let other_admin = admins.iter().any(|id| id != &account.me);
                    let others_remain = members.iter().any(|id| id != &account.me);
                    if me_admin && !other_admin && others_remain {
                        return Err(Refusal::Refused("Choose a new admin first.".to_owned()));
                    }
                }
                account.wire.leave_group(&group_id).map_err(refused)?;
                {
                    let mut live = account.live();
                    live.groups.remove(&conversation_id);
                    live.group_admins.remove(&conversation_id);
                    live.group_info.remove(&conversation_id);
                    live.known_conversations.remove(&conversation_id);
                    live.archived.remove(&conversation_id);
                    live.pinned.retain(|id| id != &conversation_id);
                }
                let _ = ctx.conversation_gone(&conversation_id);
                Ok(())
            })();
            let _ = reply.send(outcome);
        }
        Command::SetDisappearing { conversation_id, seconds, reply } => {
            if let Some(Peer::Group(group_id)) = Peer::parse(&conversation_id) {
                let allowed = account
                    .live()
                    .group_info
                    .get(&conversation_id)
                    .is_some_and(|g| group_rights(g, &account.me).edit_topic);
                if !allowed {
                    let _ = reply.send(Err(Refusal::Refused("Only group admins can change the disappearing messages timer.".into())));
                    return;
                }
                if let Err(fault) = account.wire.group_set_timer(&group_id, seconds) {
                    let _ = reply.send(Err(refused(fault)));
                    return;
                }
            }
            let next_version = {
                let mut live = account.live();
                let current = live.timer_of(&conversation_id);
                let version = current.version.saturating_add(1);
                live.note_timer(&conversation_id, seconds, version);
                version
            };
            let _ = next_version;
            let _ = ctx.disappearing(&conversation_id, (seconds > 0).then_some(seconds));
            let line = if seconds == 0 {
                "Disappearing messages are off".to_owned()
            } else {
                format!("Disappearing messages are set to {}", duration(seconds))
            };
            let sent_ms = now_ms();
            let mut msg = Message::plain(format!("signal-timer-{sent_ms}"), String::new(), line, (sent_ms / 1_000) as i64, false);
            msg.kind = MessageKind::System;
            let _ = ctx.message(&conversation_id, &msg);
            let _ = reply.send(Ok(()));
        }
        Command::CallJoin(conversation_id) => call_join(account, &conversation_id),
        Command::CallLeave(conversation_id) => call_leave(account, &conversation_id),
        Command::CallAudio { muted, deafened } => {
            if let Some(active) = account.live().active_call.as_mut() {
                active.muted = muted;
                active.deafened = deafened;
            }
            let group_update = {
                let mut live = account.live();
                live.active_group_call.as_mut().map(|g| {
                    g.muted = muted;
                    g.deafened = deafened;
                    (g.group_id.clone(), g.era_id.clone(), g.muted, g.camera, g.sharing)
                })
            };
            if let Some((group_id, era_id, muted, camera, sharing)) = group_update {
                let _ = account.wire.group_call_send_data(
                    &group_id,
                    GroupCallSignal::MediaState { group_id: group_id.clone(), era_id, muted, camera, sharing },
                );
            }
        }
        Command::CallPicture(picture) => {
            if let Some(active) = account.live().active_call.as_mut() {
                match picture {
                    Picture::Camera(on) => {
                        active.camera = on;
                        if on {
                            active.video = true;
                        }
                    }
                    Picture::Share { on, .. } => active.sharing = on,
                    Picture::Watch(_) | Picture::WatchList(_) => {}
                }
            }
            let group_update = {
                let mut live = account.live();
                live.active_group_call.as_mut().map(|g| {
                    match &picture {
                        Picture::Camera(on) => g.camera = *on,
                        Picture::Share { on, .. } => g.sharing = *on,
                        Picture::Watch(_) | Picture::WatchList(_) => {}
                    }
                    (g.group_id.clone(), g.era_id.clone(), g.muted, g.camera, g.sharing)
                })
            };
            if let Some((group_id, era_id, muted, camera, sharing)) = group_update {
                let _ = account.wire.group_call_send_data(
                    &group_id,
                    GroupCallSignal::MediaState { group_id: group_id.clone(), era_id, muted, camera, sharing },
                );
            }
        }
        Command::CallIce { conversation_id, candidate } => call_ice_send(account, &conversation_id, candidate),
        Command::CallRing { conversation_id, people, reply } => {
            let _ = reply.send(group_call_ring(account, &conversation_id, people, false));
        }
        Command::CallStopRinging { conversation_id, people, reply } => {
            let _ = reply.send(group_call_ring(account, &conversation_id, people, true));
        }
        Command::CallRaiseHand { conversation_id, raised, reply } => {
            let _ = reply.send(group_call_raise_hand(account, &conversation_id, raised));
        }
        Command::CallReact { conversation_id, emoji, reply } => {
            let _ = reply.send(group_call_react(account, &conversation_id, emoji));
        }
        other => other.unsupported(ctx),
    }
}

/// Records us as a person, so the account card can be opened. A card the wire cannot read
/// still leaves the name connect already stored.
fn show_self(account: &Account) {
    let card = account.wire.card().unwrap_or_default();
    let name = if card.name.is_empty() { account.name.clone() } else { card.name };
    if !name.is_empty() {
        let _ = account.ctx.describe(&name, &account.me);
    }
    let _ = account.ctx.people(&[PersonRow {
        id: account.me.clone(),
        name,
        handle: card.username,
        avatar: account.ctx.avatar_of(&account.me),
        link: card.link,
        ..PersonRow::default()
    }]);
}

fn own_about(account: &Account) -> Result<About, Refusal> {
    let card = account.wire.card().map_err(refused)?;
    let mut about = About { bio: card.about, ..About::default() };
    if !card.phone.is_empty() {
        about.contacts.push(Contact { kind: ContactKind::Phone, value: card.phone, masked: true });
    }
    if !card.emoji.is_empty() {
        about.fields.push(("About emoji".into(), card.emoji));
    }
    if !card.username.is_empty() {
        about.fields.push(("Username".into(), card.username));
    }
    Ok(about)
}

/// Fetches another person's Signal profile (decrypted name/about/emoji/badges, masked phone,
/// stored username, shared groups, nickname & private note, and 60-digit safety number).
fn person_about(account: &Account, person_id: &str) -> Result<About, Refusal> {
    if !wire::is_aci(person_id) {
        return Err(Refusal::Refused("That person isn't one Signal knows.".to_owned()));
    }
    let profile = account.wire.person_profile(person_id).map_err(refused)?;
    let safety = account.wire.safety_number(person_id).ok().flatten();
    let (nickname, note, phone, username, mutual_spaces, known_name, refresh_person) = {
        let mut live = account.live();
        let mut refresh = None;
        if !profile.name.is_empty() {
            live.names.insert(person_id.to_owned(), profile.name.clone());
            let display = live.known_name(person_id);
            let handle = live.usernames.get(person_id).cloned().unwrap_or_default();
            refresh = Some((display, handle));
        }
        let nickname = live.nicknames.get(person_id).cloned().unwrap_or_default();
        let note = live.notes.get(person_id).cloned().unwrap_or_default();
        let phone = live.phones.get(person_id).cloned().unwrap_or_default();
        let username = live.usernames.get(person_id).cloned().unwrap_or_default();
        let mut mutual: Vec<String> = live
            .group_info
            .values()
            .filter(|g| g.member && g.member_ids.iter().any(|id| id == person_id))
            .map(|g| if g.title.is_empty() { "Signal group".to_owned() } else { g.title.clone() })
            .collect();
        mutual.sort();
        mutual.dedup();
        let known_name = live.known_name(person_id);
        (nickname, note, phone, username, mutual, known_name, refresh)
    };
    if let Some((display, handle)) = refresh_person {
        let _ = account.ctx.people(&[PersonRow {
            id: person_id.to_owned(),
            name: if display.is_empty() { NO_NAME.to_owned() } else { display },
            handle,
            avatar: account.ctx.avatar_of(person_id),
            ..PersonRow::default()
        }]);
    }
    let bio = if !profile.about.trim().is_empty() {
        if !profile.emoji.trim().is_empty() {
            format!("{} {}", profile.emoji.trim(), profile.about.trim())
        } else {
            profile.about.trim().to_owned()
        }
    } else if !profile.key_known && known_name.is_empty() {
        "Name not shared yet".to_owned()
    } else {
        String::new()
    };
    let mutual_total = mutual_spaces.len() as u32;
    let mut about = About {
        bio,
        nickname,
        note,
        mutual_spaces,
        mutual_total,
        ..About::default()
    };
    if !phone.is_empty() {
        about.contacts.push(Contact { kind: ContactKind::Phone, value: phone, masked: true });
    }
    if !profile.emoji.trim().is_empty() {
        about.fields.push(("About emoji".into(), profile.emoji.trim().to_owned()));
    }
    if !profile.badges.is_empty() {
        about.fields.push(("Badges".into(), profile.badges.join(", ")));
    }
    if !username.is_empty() {
        about.fields.push(("Username".into(), username));
    }
    if let Some(safety) = safety {
        let formatted = wire::format_safety_number(&safety.digits);
        if !formatted.is_empty() {
            about.fields.push(("Safety number".into(), formatted));
            let status = if safety.changed {
                "Changed — approval required"
            } else {
                match safety.state {
                    wire::IdentityState::Verified => "Verified",
                    wire::IdentityState::Unverified => "Unverified",
                    wire::IdentityState::Default => "Not verified",
                }
            };
            about.fields.push(("Safety status".into(), status.to_owned()));
        }
    }
    Ok(about)
}

fn set_own(account: &Account, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Refusal> {
    if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
        return Err(Refusal::Refused("A name is needed.".into()));
    }
    account.wire.set_card(name, about, emoji).map_err(refused)?;
    show_self(account);
    Ok(())
}

/// Computes `ConversationRights` for a Signal group from its access control and our role.
fn group_rights(group: &wire::GroupInfo, me: &str) -> ConversationRights {
    if !group.member {
        return ConversationRights::default();
    }
    let is_admin = group.admins.iter().any(|id| id == me);
    let can_add = is_admin || matches!(group.member_access, wire::GroupAccess::Member | wire::GroupAccess::Any);
    let can_edit = is_admin || matches!(group.attributes_access, wire::GroupAccess::Member | wire::GroupAccess::Any);
    ConversationRights {
        pin: can_edit,
        delete_others: is_admin,
        manage_members: is_admin,
        add_members: can_add,
        remove_members: is_admin,
        rename: can_edit,
        edit_topic: can_edit,
        edit_picture: can_edit,
        manage_webhooks: false,
        create_invite: false,
        manage_topics: false,
        ..ConversationRights::default()
    }
}

fn group_members(account: &Account, conversation_id: &str, offset: u32, limit: u32) -> Result<Members, Refusal> {
    let Some(Peer::Group(group_id)) = Peer::parse(conversation_id) else {
        return Err(Refusal::Unsupported);
    };
    let listed = account.wire.group_members(&group_id).map_err(refused)?;
    let live = account.live();
    let resolve_name = |m: &wire::GroupMember| -> String {
        if m.id == account.me {
            if !account.name.is_empty() {
                return account.name.clone();
            }
        }
        if !m.name.is_empty() {
            return m.name.clone();
        }
        live.name(&m.id)
    };
    let make_person = |m: &wire::GroupMember, status: &str| -> Person {
        let mut person = Person::new(account.ctx.key(&m.id), resolve_name(m));
        person.status = status.to_owned();
        person
    };

    let mut admins: Vec<Person> = listed
        .members
        .iter()
        .filter(|m| m.admin)
        .map(|m| make_person(m, "Admin"))
        .collect();
    let mut regular: Vec<Person> = listed
        .members
        .iter()
        .filter(|m| !m.admin)
        .map(|m| make_person(m, ""))
        .collect();
    let mut pending: Vec<Person> = listed
        .pending
        .iter()
        .map(|m| make_person(m, "Invited"))
        .collect();
    let mut requesting: Vec<Person> = listed
        .requesting
        .iter()
        .map(|m| make_person(m, "Requested to join"))
        .collect();
    drop(live);

    let mut sections = Vec::new();
    if !admins.is_empty() {
        sections.push(MemberSection { name: "Admins".into(), count: admins.len() as u32 });
    }
    if !regular.is_empty() {
        sections.push(MemberSection { name: "Members".into(), count: regular.len() as u32 });
    }
    if !pending.is_empty() {
        sections.push(MemberSection { name: "Pending invites".into(), count: pending.len() as u32 });
    }
    if !requesting.is_empty() {
        sections.push(MemberSection { name: "Join requests".into(), count: requesting.len() as u32 });
    }

    let mut all = Vec::with_capacity(admins.len() + regular.len() + pending.len() + requesting.len());
    all.append(&mut admins);
    all.append(&mut regular);
    all.append(&mut pending);
    all.append(&mut requesting);

    let start = (offset as usize).min(all.len());
    let take = if limit == 0 { all.len() } else { limit as usize };
    let members = all.into_iter().skip(start).take(take).collect();
    Ok(Members { pending: false, sections, members })
}

fn conversation_info(account: &Account, conversation_id: &str) -> Result<ConversationAbout, Refusal> {
    let Some(peer) = Peer::parse(conversation_id) else {
        return Err(Refusal::Unsupported);
    };
    let live = account.live();
    match peer {
        Peer::Group(ref group_id) => {
            let peeked = account.wire.peek_group_call(group_id).ok();
            let Some(group) = live.group_info.get(conversation_id).cloned() else {
                return Err(Refusal::Refused("This group isn't one Signal still knows for you.".into()));
            };
            let timer = live.timer_of(conversation_id).seconds;
            let disappearing = if timer > 0 { Some(timer) } else if group.timer_secs > 0 { Some(group.timer_secs) } else { None };
            drop(live);
            if let Some(info) = peeked {
                if !info.era_id.is_empty() || !info.members.is_empty() {
                    apply_group_call_roster(account, group_id, info);
                }
            }
            Ok(ConversationAbout {
                description: group.description,
                members: Some(group.members),
                link: group.invite_link,
                disappearing,
                ..ConversationAbout::default()
            })
        }
        Peer::Direct(_) => {
            let timer = live.timer_of(conversation_id).seconds;
            Ok(ConversationAbout {
                disappearing: (timer > 0).then_some(timer),
                ..ConversationAbout::default()
            })
        }
    }
}

fn members_add(account: &Account, conversation_id: &str, people: &[String]) -> Result<(), Refusal> {
    let Some(Peer::Group(group_id)) = Peer::parse(conversation_id) else {
        return Err(Refusal::Unsupported);
    };
    if people.is_empty() {
        return Err(Refusal::Refused("Choose at least one person to add.".into()));
    }
    let allowed = account
        .live()
        .group_info
        .get(conversation_id)
        .is_some_and(|g| group_rights(g, &account.me).add_members);
    if !allowed {
        return Err(Refusal::Refused("Only group admins can add members.".into()));
    }
    let mut acis = Vec::with_capacity(people.len());
    for raw in people {
        let trimmed = raw.trim();
        if wire::is_aci(trimmed) {
            acis.push(trimmed.to_owned());
        } else if let Some(username) = username_of(trimmed) {
            match account.wire.lookup(username).map_err(refused)? {
                Some(aci) if wire::is_aci(&aci) => acis.push(aci),
                _ => return Err(Refusal::Refused(format!("No Signal account found for {trimmed}."))),
            }
        } else {
            return Err(Refusal::Refused(format!("{trimmed} is not a Signal account id or username.")));
        }
    }
    account.wire.group_add_members(&group_id, &acis).map_err(refused)
}

fn members_remove(account: &Account, conversation_id: &str, person: &str) -> Result<(), Refusal> {
    let Some(Peer::Group(group_id)) = Peer::parse(conversation_id) else {
        return Err(Refusal::Unsupported);
    };
    if let Some((target_aci, admin)) = group_admin_action(person) {
        let target_aci = target_aci.trim();
        if !wire::is_aci(target_aci) {
            return Err(Refusal::Refused("That person's role can't be changed here.".into()));
        }
        let allowed = account
            .live()
            .group_info
            .get(conversation_id)
            .is_some_and(|g| group_rights(g, &account.me).manage_members);
        if !allowed {
            return Err(Refusal::Refused("Only group admins can change admin roles.".into()));
        }
        return account.wire.group_set_admin(&group_id, target_aci, admin).map_err(refused);
    }
    if let Some(target_aci) = person.strip_prefix(GROUP_JOIN_APPROVE).or_else(|| person.strip_prefix(GROUP_JOIN_DENY)) {
        let approve = person.starts_with(GROUP_JOIN_APPROVE);
        let target_aci = target_aci.trim();
        if !wire::is_aci(target_aci) {
            return Err(Refusal::Refused("That join request can't be reviewed here.".into()));
        }
        let allowed = account
            .live()
            .group_info
            .get(conversation_id)
            .is_some_and(|g| group_rights(g, &account.me).manage_members);
        if !allowed {
            return Err(Refusal::Refused("Only group admins can approve or deny join requests.".into()));
        }
        return account.wire.group_review_request(&group_id, target_aci, approve).map_err(refused);
    }
    let person = person.trim();
    if !wire::is_aci(person) {
        return Err(Refusal::Refused("That person can't be removed from here.".into()));
    }
    if person == account.me {
        return Err(Refusal::Refused("You can't remove yourself from the group here.".into()));
    }
    let allowed = account
        .live()
        .group_info
        .get(conversation_id)
        .is_some_and(|g| group_rights(g, &account.me).remove_members);
    if !allowed {
        return Err(Refusal::Refused("Only group admins can remove members.".into()));
    }
    account.wire.group_remove_member(&group_id, person).map_err(refused)
}

fn group_update(account: &Account, conversation_id: &str, title: Option<String>, description: Option<String>) -> Result<(), Refusal> {
    let Some(Peer::Group(group_id)) = Peer::parse(conversation_id) else {
        return Err(Refusal::Unsupported);
    };
    if let Some(desc) = description.as_deref() {
        if let Some(mode_str) = desc.strip_prefix(GROUP_LINK_PREFIX) {
            let is_admin = account
                .live()
                .group_info
                .get(conversation_id)
                .is_some_and(|g| group_rights(g, &account.me).manage_members);
            if !is_admin {
                return Err(Refusal::Refused("Only group admins can change the group invite link.".into()));
            }
            let mode = match mode_str.trim() {
                "off" => wire::InviteLinkMode::Off,
                "on" => wire::InviteLinkMode::Enabled,
                "approval" => wire::InviteLinkMode::ApprovalRequired,
                "reset" => wire::InviteLinkMode::Reset,
                _ => return Err(Refusal::Refused("Unknown invite link setting.".into())),
            };
            account.wire.group_invite_link(&group_id, mode).map_err(refused)?;
            return Ok(());
        }
        if let Some(edit) = parse_group_permissions(desc) {
            let is_admin = account
                .live()
                .group_info
                .get(conversation_id)
                .is_some_and(|g| group_rights(g, &account.me).manage_members);
            if !is_admin {
                return Err(Refusal::Refused("Only group admins can change group permissions.".into()));
            }
            return account.wire.group_update(&group_id, edit).map_err(refused);
        }
    }
    let allowed = account
        .live()
        .group_info
        .get(conversation_id)
        .is_some_and(|g| {
            let r = group_rights(g, &account.me);
            (title.is_none() || r.rename) && (description.is_none() || r.edit_topic)
        });
    if !allowed {
        return Err(Refusal::Refused("Only group admins can edit group details.".into()));
    }
    if title.as_ref().is_some_and(|t| t.trim().is_empty()) {
        return Err(Refusal::Refused("A group name can't be empty.".into()));
    }
    let edit = wire::GroupEdit { title, description, ..wire::GroupEdit::default() };
    account.wire.group_update(&group_id, edit).map_err(refused)
}

fn group_picture(account: &Account, conversation_id: &str, path: &str) -> Result<(), Refusal> {
    let Some(Peer::Group(group_id)) = Peer::parse(conversation_id) else {
        return Err(Refusal::Unsupported);
    };
    let allowed = account
        .live()
        .group_info
        .get(conversation_id)
        .is_some_and(|g| group_rights(g, &account.me).edit_picture);
    if !allowed {
        return Err(Refusal::Refused("Only group admins can change the group photo.".into()));
    }
    let bytes = if path.trim().is_empty() {
        Vec::new()
    } else {
        std::fs::read(path).map_err(|_| Refusal::Refused("The photo file could not be read.".into()))?
    };
    account.wire.group_set_avatar(&group_id, bytes).map_err(refused)
}

fn prune_expired_pins(account: &Account) {
    let now = crate::hub::now();
    let expired = account.live().prune_expired_pins(now);
    for (conv, msg_id) in expired {
        let _ = account.ctx.pinned(&conv, &msg_id, false);
        if let Ok(Some(mut stored)) = account.ctx.stored_message(&conv, &msg_id) {
            stored.pinned = false;
            stored.pinned_by = None;
            stored.pinned_at = None;
            let _ = account.ctx.message(&conv, &stored);
        }
    }
}

fn pinned_messages(account: &Account, conversation_id: &str) -> Result<Vec<Message>, Refusal> {
    let _ = peer(conversation_id).map_err(refused)?;
    prune_expired_pins(account);
    let records = account.live().pins_for(conversation_id);
    let mut out = Vec::with_capacity(records.len());
    for rec in records {
        if let Ok(Some(mut msg)) = account.ctx.stored_message(conversation_id, &rec.message_id) {
            msg.pinned = true;
            if !rec.pinned_by.is_empty() {
                msg.pinned_by = Some(rec.pinned_by);
            }
            if rec.pinned_at > 0 {
                msg.pinned_at = Some(rec.pinned_at);
            }
            out.push(msg);
        }
    }
    Ok(out)
}

fn pin_message(account: &Account, conversation_id: &str, message_id: &str, pinned: bool) -> Result<(), Refusal> {
    let to = peer(conversation_id).map_err(refused)?;
    let (sent, author) = target(message_id).map_err(refused)?;
    if matches!(to, Peer::Group(_)) {
        let allowed = account
            .live()
            .group_info
            .get(conversation_id)
            .is_some_and(|g| group_rights(g, &account.me).pin);
        if !allowed {
            return Err(Refusal::Refused("Only group admins can pin or unpin messages.".into()));
        }
    }
    let body = if pinned {
        Body::Pin { author, target: sent, duration_secs: None }
    } else {
        Body::Unpin { author, target: sent }
    };
    account.wire.send(&to, body).map_err(refused)?;
    let now = crate::hub::now();
    if pinned {
        account.live().record_pin(conversation_id, message_id, "You".to_owned(), now, None);
    } else {
        account.live().remove_pin(conversation_id, message_id);
    }
    let _ = account.ctx.pinned(conversation_id, message_id, pinned);
    if let Ok(Some(mut stored)) = account.ctx.stored_message(conversation_id, message_id) {
        stored.pinned = pinned;
        stored.pinned_by = pinned.then(|| "You".to_owned());
        stored.pinned_at = pinned.then_some(now);
        let _ = account.ctx.message(conversation_id, &stored);
    }
    if pinned {
        let sent_ms = now_ms();
        let mut sys = Message::plain(
            format!("signal-pin-{sent_ms}-{sent}"),
            String::new(),
            "You pinned a message".to_owned(),
            (sent_ms / 1_000) as i64,
            true,
        );
        sys.kind = MessageKind::System;
        let _ = account.ctx.message(conversation_id, &sys);
    }
    Ok(())
}

fn catalog(account: &Account, kind: CatalogKind, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Refusal> {
    if kind == CatalogKind::Stickers {
        let q = query.trim().to_ascii_lowercase();
        let packs = {
            let mut live = account.live();
            if !live.stickers_loaded {
                live.sticker_packs = read_sticker_packs(&live.stickers_path);
                live.stickers_loaded = true;
            }
            live.sticker_packs.clone()
        };
        let mut items = Vec::new();
        let mut media = Vec::new();
        {
            let mut live = account.live();
            if live.held_stickers.len() > 2_048 {
                live.held_stickers.clear();
            }
            for pack in packs {
                let pack_matches = q.is_empty()
                    || pack.title.to_ascii_lowercase().contains(&q)
                    || pack.author.to_ascii_lowercase().contains(&q);
                for entry in pack.stickers {
                    if !pack_matches && !entry.emoji.to_ascii_lowercase().contains(&q) {
                        continue;
                    }
                    let tile_id = format!("signal:sticker:{}:{}", pack.id, entry.id);
                    let address = format!("signal://sticker/{}/{}", pack.id, entry.id);
                    let mut file = MediaFile::new(tile_id.clone(), MediaKind::Image);
                    file.width = 512;
                    file.height = 512;
                    media.push((file.clone(), address.clone()));
                    live.held_stickers.insert(
                        tile_id,
                        HeldSticker {
                            pack_id: pack.id.clone(),
                            pack_key: pack.key.clone(),
                            sticker_id: entry.id,
                            file: file.clone(),
                            address,
                        },
                    );
                    items.push(CatalogItem {
                        id: format!("{}:{}", pack.id, entry.id),
                        name: entry.emoji,
                        file: Some(file),
                        pack: pack.title.clone(),
                        animated: entry.content_type.contains("apng") || entry.content_type.contains("gif"),
                        set: String::new(),
                        locked: false,
                    });
                    if items.len() >= protocol::CATALOG_LIMIT {
                        break;
                    }
                }
                if items.len() >= protocol::CATALOG_LIMIT {
                    break;
                }
            }
        }
        if !media.is_empty() {
            let _ = account.ctx.media(&media);
        }
        return Ok(CatalogPage { items, cursor: None });
    }
    if kind != CatalogKind::Gifs {
        return Err(Refusal::Unsupported);
    }
    let offset = cursor.and_then(|c| c.parse::<u32>().ok()).unwrap_or(0);
    let (entries, next_offset) = account.wire.search_gifs(query, offset).map_err(refused)?;
    let mut items = Vec::with_capacity(entries.len());
    let mut media = Vec::with_capacity(entries.len());
    {
        let mut live = account.live();
        if live.gifs.len() > 256 {
            live.gifs.clear();
        }
        for entry in entries {
            let mut file = MediaFile::new(format!("signal:gif:{}", entry.id), MediaKind::Gif);
            file.width = entry.width;
            file.height = entry.height;
            if !entry.preview_url.is_empty() {
                media.push((file.clone(), entry.preview_url.clone()));
            }
            items.push(CatalogItem {
                id: entry.id.clone(),
                name: entry.title.clone(),
                file: Some(file),
                pack: String::new(),
                animated: true,
                set: String::new(),
                locked: false,
            });
            live.gifs.insert(entry.id.clone(), entry);
        }
    }
    if !media.is_empty() {
        let _ = account.ctx.media(&media);
    }
    Ok(CatalogPage { items, cursor: next_offset.map(|n| n.to_string()) })
}

fn call_link_handoff(account: &Account, raw: &str) -> Result<String, Refusal> {
    if let Some(rest) = raw.strip_prefix(CALL_LINK_CREATE) {
        let (approve_str, name) = rest.split_once('|').unwrap_or(("0", rest));
        let approve_all = approve_str == "1" || approve_str.eq_ignore_ascii_case("true");
        let link = account.wire.create_call_link(name.trim(), approve_all).map_err(refused)?;
        let url = link.url.clone();
        account.live().call_links.insert(link.root_key, link);
        return Ok(url);
    }
    if let Some(rest) = raw.strip_prefix(CALL_LINK_UPDATE) {
        let mut parts = rest.splitn(3, '|');
        let approve_str = parts.next().unwrap_or("");
        let url = parts.next().unwrap_or("").trim();
        let name = parts.next().map(|s| s.trim().to_owned());
        let Some(root_key) = wire::parse_call_link(url) else {
            return Err(Refusal::Refused("That is not a valid Signal call link.".into()));
        };
        let approve = match approve_str {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        };
        let updated = account.wire.update_call_link(&root_key, name, approve).map_err(refused)?;
        let url = updated.url.clone();
        account.live().call_links.insert(updated.root_key, updated);
        return Ok(url);
    }
    if let Some(url) = raw.strip_prefix(CALL_LINK_JOIN) {
        let joined = account.wire.join_call_link(url.trim()).map_err(refused)?;
        let prefix = if joined.waiting_for_approval { "waiting" } else { "room" };
        return Ok(format!("{prefix}:{}", joined.room_id));
    }
    if let Some(url) = raw.strip_prefix(CALL_LINK_DELETE) {
        let Some(root_key) = wire::parse_call_link(url.trim()) else {
            return Err(Refusal::Refused("That is not a valid Signal call link.".into()));
        };
        account.wire.delete_call_link(&root_key).map_err(refused)?;
        account.live().call_links.remove(&root_key);
        return Ok("deleted".to_owned());
    }
    if let Some(aci) = raw.strip_prefix(SAFETY_VERIFY) {
        return set_safety_state(account, aci.trim(), wire::IdentityState::Verified);
    }
    if let Some(aci) = raw.strip_prefix(SAFETY_APPROVE) {
        return set_safety_state(account, aci.trim(), wire::IdentityState::Default);
    }
    if let Some(aci) = raw.strip_prefix(SAFETY_UNVERIFY) {
        return set_safety_state(account, aci.trim(), wire::IdentityState::Default);
    }
    if let Some(arg) = raw.strip_prefix(STICKER_PACK_PREVIEW) {
        let Some((pack_id, pack_key)) = parse_sticker_pack_arg(arg) else {
            return Err(Refusal::Refused("That is not a valid Signal sticker pack link.".into()));
        };
        let info = account.wire.preview_sticker_pack(&pack_id, &pack_key).map_err(refused)?;
        return serde_json::to_string(&info).map_err(|err| Refusal::Refused(err.to_string()));
    }
    if let Some(arg) = raw.strip_prefix(STICKER_PACK_INSTALL) {
        let Some((pack_id, pack_key)) = parse_sticker_pack_arg(arg) else {
            return Err(Refusal::Refused("That is not a valid Signal sticker pack link.".into()));
        };
        let mut info = account.wire.install_sticker_pack(&pack_id, &pack_key).map_err(refused)?;
        info.installed = true;
        {
            let mut live = account.live();
            if !live.stickers_loaded {
                live.sticker_packs = read_sticker_packs(&live.stickers_path);
                live.stickers_loaded = true;
            }
            if let Some(existing) = live.sticker_packs.iter_mut().find(|p| p.id == info.id) {
                *existing = info.clone();
            } else if live.sticker_packs.len() < 128 {
                live.sticker_packs.push(info.clone());
            }
            write_sticker_packs(&live.stickers_path, &live.sticker_packs);
        }
        account.ctx.hub.catalogs.forget_matching(|(acct, kind, ..)| *acct == account.ctx.account && *kind == CatalogKind::Stickers);
        return serde_json::to_string(&info).map_err(|err| Refusal::Refused(err.to_string()));
    }
    if let Some(pack_id) = raw.strip_prefix(STICKER_PACK_REMOVE) {
        let id = pack_id.trim().to_ascii_lowercase();
        let pack_key = {
            let mut live = account.live();
            if !live.stickers_loaded {
                live.sticker_packs = read_sticker_packs(&live.stickers_path);
                live.stickers_loaded = true;
            }
            live.sticker_packs.iter().find(|p| p.id == id).map(|p| p.key.clone()).unwrap_or_default()
        };
        account.wire.remove_sticker_pack(&id, &pack_key).map_err(refused)?;
        {
            let mut live = account.live();
            live.sticker_packs.retain(|p| p.id != id);
            write_sticker_packs(&live.stickers_path, &live.sticker_packs);
        }
        account.ctx.hub.catalogs.forget_matching(|(acct, kind, ..)| *acct == account.ctx.account && *kind == CatalogKind::Stickers);
        return Ok("removed".to_owned());
    }
    if let Some(arg) = raw.strip_prefix(STORY_VIEW) {
        let mut parts = arg.splitn(2, '|');
        let author = parts.next().unwrap_or("").trim();
        let sent: u64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| Refusal::Refused("Invalid story timestamp.".into()))?;
        if !wire::is_aci(author) {
            return Err(Refusal::Refused("Invalid story author.".into()));
        }
        let story_id = wire::message_id(sent, author);
        let send_receipts = {
            let live = account.live();
            if live.stories_disabled {
                return Err(Refusal::Refused("Stories are turned off.".into()));
            }
            live.story_view_receipts
        };
        if visible_story(account, &story_id).is_none() {
            return Err(Refusal::Refused("That story is no longer available.".into()));
        }
        if author != account.me {
            account
                .wire
                .viewed(&[(author.to_owned(), sent)], send_receipts)
                .map_err(refused)?;
        }
        return Ok("viewed".to_owned());
    }
    if let Some(arg) = raw.strip_prefix(STORY_REPLY) {
        let mut parts = arg.splitn(4, '|');
        let author = parts.next().unwrap_or("").trim();
        let sent: u64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| Refusal::Refused("Invalid story timestamp.".into()))?;
        // The handoff's group is not the destination. Only the held story's group is.
        let _group = parts.next().unwrap_or("");
        let reply_text = parts.next().unwrap_or("").trim();
        if !wire::is_aci(author) || reply_text.is_empty() {
            return Err(Refusal::Refused("A story reply needs text.".into()));
        }
        let story_id = wire::message_id(sent, author);
        if account.live().stories_disabled {
            return Err(Refusal::Refused("Stories are turned off.".into()));
        }
        let Some(story) = visible_story(account, &story_id) else {
            return Err(Refusal::Refused("That story is no longer available.".into()));
        };
        if !story.allows_replies {
            return Err(Refusal::Refused("Replies are turned off for this story.".into()));
        }
        let snippet = if story.snippet.is_empty() { "Story".to_owned() } else { story.snippet.clone() };
        let to = match story.group_id.as_deref() {
            Some(gid) => Peer::Group(gid.to_owned()),
            None => Peer::Direct(author.to_owned()),
        };
        let conv = to.conversation();
        let timer = account.live().timer_of(&conv);
        let (wire_body, ranges, shown) = format_outgoing_body(account, &to, reply_text, &[]);
        let reply_sent = account
            .wire
            .send(
                &to,
                Body::StoryReply {
                    story_author: author.to_owned(),
                    story_sent: sent,
                    text: wire_body,
                    ranges,
                    timer,
                },
            )
            .map_err(refused)?;
        let target_name = if author == account.me { account.name.clone() } else { account.live().name(author) };
        ensure_row(account, &to, &target_name);
        let msg_id = wire::message_id(reply_sent, &account.me);
        let mut msg = Message::plain(msg_id.clone(), account.name.clone(), shown.text, (reply_sent / 1_000) as i64, true);
        msg.sender.id = account.me.clone();
        msg.spans = shown.spans;
        msg.reply_to = Some(ReplyTo {
            id: story_id,
            sender: target_name,
            snippet: snippet.chars().take(200).collect(),
            person: Some(account.ctx.key(author)),
            deleted: false,
            thumb: None,
            story: true,
        });
        let _ = account.ctx.message(&conv, &msg);
        return Ok(msg_id);
    }
    if let Some(arg) = raw.strip_prefix(STORY_REACT) {
        let mut parts = arg.splitn(4, '|');
        let author = parts.next().unwrap_or("").trim();
        let sent: u64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| Refusal::Refused("Invalid story timestamp.".into()))?;
        // The handoff's group is not the destination. Only the held story's group is.
        let _group = parts.next().unwrap_or("");
        let emoji = parts.next().unwrap_or("").trim();
        if !wire::is_aci(author) || emoji.is_empty() {
            return Err(Refusal::Refused("A story reaction needs an emoji.".into()));
        }
        if account.live().stories_disabled {
            return Err(Refusal::Refused("Stories are turned off.".into()));
        }
        let story_id = wire::message_id(sent, author);
        let Some(story) = visible_story(account, &story_id) else {
            return Err(Refusal::Refused("That story is no longer available.".into()));
        };
        let to = match story.group_id.as_deref() {
            Some(gid) => Peer::Group(gid.to_owned()),
            None => Peer::Direct(author.to_owned()),
        };
        account
            .wire
            .send(
                &to,
                Body::StoryReact {
                    story_author: author.to_owned(),
                    story_sent: sent,
                    emoji: emoji.to_owned(),
                    remove: false,
                },
            )
            .map_err(refused)?;
        let _ = account.ctx.reacted(
            STORIES_CONVERSATION,
            &story_id,
            &Reaction { emoji: emoji.to_owned(), file: None, count: 1, mine: true, who: vec![account.me.clone()], burst: 0, mine_burst: false, burst_colors: Vec::new() },
            true,
        );
        return Ok(story_id);
    }
    if let Some(arg) = raw.strip_prefix(STORY_POST) {
        let mut parts = arg.splitn(4, '|');
        let target = parts.next().unwrap_or("").trim();
        let allows_replies = parts.next().unwrap_or("1").trim() != "0";
        let bg_hex = parts.next().unwrap_or("224488").trim().trim_start_matches('#');
        let text = parts.next().unwrap_or("").trim();
        if text.is_empty() {
            return Err(Refusal::Refused("A text story can't be empty.".into()));
        }
        let bg_color = u32::from_str_radix(bg_hex, 16).unwrap_or(0x0022_4488) | 0xff00_0000;
        let (group_id, recipients) = {
            let live = account.live();
            if live.stories_disabled {
                return Err(Refusal::Refused("Stories are turned off.".into()));
            }
            if let Some(Peer::Group(gid)) = Peer::parse(target) {
                (Some(gid), Vec::new())
            } else {
                // No matching list: do not invent "My Story" with an empty blocklist. That sends to everyone.
                let Some(list) = live
                    .story_lists
                    .iter()
                    .find(|l| {
                        l.id.eq_ignore_ascii_case(target)
                            || l.name.eq_ignore_ascii_case(target)
                            || (target.is_empty() || target.eq_ignore_ascii_case("my-story"))
                                && (l.id == "00000000000000000000000000000000" || l.name.eq_ignore_ascii_case("My Story"))
                    })
                    .cloned()
                else {
                    return Err(Refusal::Refused("That story audience isn't on this account.".into()));
                };
                let accepted: Vec<String> = live.accepted.iter().cloned().collect();
                let resolved = wire::resolve_story_recipients(&list, &accepted, &live.blocked, &account.me);
                (None, resolved)
            }
        };
        if group_id.is_none() && recipients.is_empty() {
            return Err(Refusal::Refused("No recipients in this story audience.".into()));
        }
        let story_text = wire::StoryText {
            text: text.to_owned(),
            style: "BOLD".to_owned(),
            fg_color: 0xffff_ffff,
            bg_color,
            gradient_colors: Vec::new(),
            preview: None,
        };
        let direct = group_id.is_none();
        let peer_anchor = match &group_id {
            Some(gid) => Peer::Group(gid.clone()),
            None => Peer::Direct(account.me.clone()),
        };
        let posted_ms = account
            .wire
            .send(
                &peer_anchor,
                Body::PostStory {
                    recipients: recipients.clone(),
                    group_id: group_id.clone(),
                    text: Some(story_text.clone()),
                    file: None,
                    allows_replies,
                },
            )
            .map_err(refused)?;
        apply_story(
            account,
            wire::StoryMessageData {
                author: account.me.clone(),
                name: account.name.clone(),
                sent: posted_ms,
                group_id,
                allows_replies,
                file: None,
                text: Some(story_text),
                outgoing: true,
            },
        );
        let id = wire::message_id(posted_ms, &account.me);
        if let Some(held) = account.live().stories.get_mut(&id) {
            held.audience = if direct { recipients } else { Vec::new() };
        }
        return Ok(id);
    }
    if let Some(arg) = raw.strip_prefix(STORY_TOGGLE) {
        let on = arg.trim().eq_ignore_ascii_case("on") || arg.trim() == "1" || arg.trim().eq_ignore_ascii_case("true");
        {
            let mut live = account.live();
            live.stories_disabled = !on;
            if !on {
                live.stories.clear();
            }
        }
        if !on {
            let _ = account.ctx.forget_conversation(STORIES_CONVERSATION);
            return Ok("stories_off".to_owned());
        }
        return Ok("stories_on".to_owned());
    }
    Err(Refusal::Unsupported)
}

fn parse_sticker_pack_arg(arg: &str) -> Option<(String, String)> {
    let trimmed = arg.trim();
    if let Some(pair) = wire::parse_sticker_pack_url(trimmed) {
        return Some(pair);
    }
    let (id, key) = trimmed.split_once('|')?;
    let id = id.trim();
    let key = key.trim();
    if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some((id.to_ascii_lowercase(), key.to_ascii_lowercase()))
    } else {
        None
    }
}

fn set_safety_state(account: &Account, aci: &str, state: wire::IdentityState) -> Result<String, Refusal> {
    if !wire::is_aci(aci) {
        return Err(Refusal::Refused("That person isn't one Signal knows.".to_owned()));
    }
    account.wire.set_verified(aci, state).map_err(refused)?;
    {
        let mut live = account.live();
        live.identity_states.insert(aci.to_owned(), state);
        live.identity_changed.remove(aci);
    }
    let mark = (state == wire::IdentityState::Verified).then_some(Mark::Verified);
    let _ = account.ctx.details(&[(aci.to_owned(), PersonDetails { mark, ..PersonDetails::default() })]);
    let formatted = account
        .wire
        .safety_number(aci)
        .ok()
        .flatten()
        .map(|s| wire::format_safety_number(&s.digits))
        .unwrap_or_default();
    Ok(formatted)
}

fn send_gif(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, pick_id: &str) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let entry = account.live().gifs.get(pick_id).cloned();
    let (mp4_url, width, height) = match entry {
        Some(entry) => (entry.mp4_url, entry.width, entry.height),
        None if pick_id.starts_with("https://") => (pick_id.to_owned(), 0, 0),
        None => return Err(Fault::Refused("That GIF is no longer in the picker. Search again.".into())),
    };
    let bytes = account.wire.fetch_gif(&mp4_url)?;
    if let Some(why) = files::too_big("giphy.mp4", bytes.len() as u64) {
        return Err(Fault::Refused(why));
    }
    let gif_file = wire::File {
        name: "giphy.mp4".to_owned(),
        mime: "video/mp4".to_owned(),
        bytes,
        width,
        height,
        thumb: Vec::new(),
        blurhash: None,
        gif: true,
        voice: false,
    };
    let quote = quote_of(account, conversation, reply_to)?;
    let attachment = show_file(account, client_id, 0, &gif_file);
    let timer = account.live().timer_of(conversation);
    let (wire_body, ranges, shown) = format_outgoing_body(account, &to, text, &[]);
    let sent = match account.wire.send(&to, Body::Files { text: wire_body, quote: quote.clone(), ranges, files: vec![gif_file], timer }) {
        Ok(sent) => sent,
        Err(fault) => {
            forget_landed(&[attachment]);
            return Err(fault);
        }
    };
    account.live().echoed(sent);
    account.live().sent(sent, conversation);
    let words = Text {
        author: account.me.clone(),
        name: account.name.clone(),
        sent,
        text: shown.text,
        spans: shown.spans,
        quote,
        ..Text::default()
    };
    let mut message = message(account, conversation, &words, true);
    if timer.seconds > 0 {
        message.expires_at = Some(crate::hub::now() + i64::from(timer.seconds));
    }
    message.attachments = vec![attachment];
    Ok(message)
}

fn send_sticker(
    account: &Account,
    conversation: &str,
    client_id: &str,
    reply_to: Option<&str>,
    pick_id: &str,
) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let (pack_id, sticker_id_str) = pick_id
        .split_once(':')
        .ok_or_else(|| Fault::Refused("That sticker isn't installed.".to_owned()))?;
    let sticker_id: u32 = sticker_id_str
        .parse()
        .map_err(|_| Fault::Refused("That sticker isn't installed.".to_owned()))?;
    let pack = {
        let mut live = account.live();
        if !live.stickers_loaded {
            live.sticker_packs = read_sticker_packs(&live.stickers_path);
            live.stickers_loaded = true;
        }
        live.sticker_packs.iter().find(|p| p.id == pack_id).cloned()
    }
    .ok_or_else(|| Fault::Refused("That sticker pack isn't installed.".to_owned()))?;
    let entry = pack.stickers.iter().find(|s| s.id == sticker_id);
    let emoji = entry.map(|s| s.emoji.clone()).unwrap_or_default();
    let content_type = entry
        .map(|s| s.content_type.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "image/webp".to_owned());
    let tmp = tempfile::NamedTempFile::new().map_err(|err| Fault::Net(err.to_string()))?;
    account
        .wire
        .download_sticker(&pack.id, &pack.key, sticker_id, tmp.path())?;
    let bytes = std::fs::read(tmp.path()).map_err(|err| Fault::Net(err.to_string()))?;
    let quote = quote_of(account, conversation, reply_to)?;
    let timer = account.live().timer_of(conversation);
    let sent = account.wire.send(
        &to,
        Body::Sticker {
            pack_id: pack.id.clone(),
            pack_key: pack.key.clone(),
            sticker_id,
            emoji: emoji.clone(),
            bytes: bytes.clone(),
            content_type: content_type.clone(),
            quote: quote.clone(),
            timer,
        },
    )?;
    account.live().echoed(sent);
    account.live().sent(sent, conversation);
    let mut media = MediaFile::new(format!("signal:{client_id}:sticker"), MediaKind::Image);
    media.width = 512;
    media.height = 512;
    media.bytes = bytes.len() as u64;
    if let Some(landed) = account.ctx.landed(&media, &format!("signal://sticker/{}/{sticker_id}", pack.id), &bytes) {
        media = landed;
    }
    let name = if !emoji.is_empty() { emoji } else { "sticker.webp".to_owned() };
    let mut attachment = Attachment::new(name, media);
    attachment.mime = content_type;
    attachment.sticker = true;
    let words = Text {
        author: account.me.clone(),
        name: account.name.clone(),
        sent,
        quote,
        ..Text::default()
    };
    let mut message = message(account, conversation, &words, true);
    if timer.seconds > 0 {
        message.expires_at = Some(crate::hub::now() + i64::from(timer.seconds));
    }
    message.attachments = vec![attachment];
    Ok(message)
}

/// Streams and decrypts wanted Signal attachments and sticker tiles off the command thread into chatd's media cache.
fn wanted(account: &Arc<Account>, ids: &[String]) {
    let (jobs, sticker_jobs): (Vec<(String, HeldAttachment)>, Vec<(String, HeldSticker)>) = {
        let mut live = account.live();
        live.load_attachments(false);
        if ids.iter().any(|id| !live.held_attachments.contains_key(id)) {
            live.load_attachments(true);
        }
        let jobs = ids
            .iter()
            .filter_map(|id| live.held_attachments.get(id).cloned().map(|h| (id.clone(), h)))
            .collect();
        let sticker_jobs = ids
            .iter()
            .filter_map(|id| live.held_stickers.get(id).cloned().map(|h| (id.clone(), h)))
            .collect();
        (jobs, sticker_jobs)
    };
    if jobs.is_empty() && sticker_jobs.is_empty() {
        return;
    }
    let account = Arc::clone(account);
    let spawned = std::thread::Builder::new().name("signal-media".into()).spawn(move || {
        let media_root = protocol::support_dir().join("media");
        for (id, held) in jobs {
            let dest = crate::blobs::path_of(&media_root, &account.ctx.account, &id).with_extension("sigpart");
            match account.wire.download_attachment(&held.pointer, &dest) {
                Ok(bytes) => {
                    if let Some(landed) = account.ctx.landed_from_part(&held.file, &held.address, &dest, bytes) {
                        if held.pointer.is_voice() {
                            if let Some(path) = landed.path.as_deref() {
                                if let Ok(raw) = std::fs::read(path) {
                                    let (duration, waveform) = files::voice_facts(&held.pointer.content_type, &raw);
                                    if let Ok(Some(mut msg)) = account.ctx.stored_message(&held.conversation, &held.message_id) {
                                        if let Some(att) = msg.attachments.iter_mut().find(|a| a.file.id == id) {
                                            att.duration = duration;
                                            att.waveform = waveform;
                                            att.file.path = Some(path.to_owned());
                                            att.file.bytes = bytes;
                                            let _ = account.ctx.message(&held.conversation, &msg);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(Fault::Refused(why)) => {
                    let _ = std::fs::remove_file(&dest);
                    account.ctx.media_failed(&id, &why);
                }
                Err(fault) => {
                    let _ = std::fs::remove_file(&dest);
                    tracing::warn!("chatd: Signal: attachment {id}: {}", super::net::redact(&fault.to_string()));
                }
            }
        }
        for (id, held) in sticker_jobs {
            let dest = crate::blobs::path_of(&media_root, &account.ctx.account, &id).with_extension("sigpart");
            match account.wire.download_sticker(&held.pack_id, &held.pack_key, held.sticker_id, &dest) {
                Ok(bytes) => {
                    let _ = account.ctx.landed_from_part(&held.file, &held.address, &dest, bytes);
                }
                Err(Fault::Refused(why)) => {
                    let _ = std::fs::remove_file(&dest);
                    account.ctx.media_failed(&id, &why);
                }
                Err(fault) => {
                    let _ = std::fs::remove_file(&dest);
                    tracing::warn!("chatd: Signal: sticker {id}: {}", super::net::redact(&fault.to_string()));
                }
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("chatd: Signal: media thread: {err}");
    }
}

/// Answers the shared Media / Files / Links tab from chatd's local message store for Signal.
fn shared(
    account: &Account,
    conversation: &str,
    kind: protocol::SharedKind,
    before: Option<&str>,
    limit: u32,
) -> Result<SharedPage, Refusal> {
    let key = account.ctx.key(conversation);
    match account.ctx.hub.store().shared(&key, kind, before, limit) {
        Ok(Some((items, more))) => Ok(SharedPage { items, more, total: None }),
        Ok(None) => Err(Refusal::Refused("no such conversation".into())),
        Err(err) => Err(Refusal::Refused(err.to_string())),
    }
}

/// The pictures of the people and groups on screen (rows, people, the open conversation): each
/// asked of Signal once a connection, one after another, off the command thread since each is a
/// trip to Signal's CDN. What lands is the row's and the person's avatar.
fn pictures(account: &Arc<Account>, watching: &protocol::Watching) {
    let on_screen = watching.rows.iter().chain(&watching.people).chain(&watching.conversation);
    let wanted: Vec<Peer> = {
        let mut pictured = account.pictured.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        on_screen
            .filter(|key| key.account() == account.ctx.account)
            .filter_map(|key| Peer::parse(&key.id))
            .filter(|peer| pictured.insert(peer.conversation()))
            .take(PICTURES)
            .collect()
    };
    if wanted.is_empty() {
        return;
    }
    let account = account.clone();
    let spawned = std::thread::Builder::new().name("signal-pictures".into()).spawn(move || {
        for peer in wanted {
            let bytes = match account.wire.picture(&peer) {
                Ok(Some(bytes)) if crate::blobs::is_picture(&bytes) => bytes,
                Ok(_) => continue,
                // Asked again on the next connection.
                Err(fault) => {
                    tracing::warn!("chatd: Signal: picture: {}", super::net::redact(&fault.to_string()));
                    continue;
                }
            };
            let conversation = peer.conversation();
            // A new picture is a new id, so the shell never draws the old one in its place.
            let id = format!("signal:avatar:{conversation}:{:016x}", crate::blobs::fnv_bytes(&bytes));
            let file = protocol::MediaFile::new(id.clone(), protocol::MediaKind::Avatar);
            if account.ctx.landed(&file, &format!("signal://avatar/{conversation}"), &bytes).is_some() {
                let _ = account.ctx.avatar(&conversation, &id);
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("chatd: Signal: {err}");
    }
}

fn refused(fault: Fault) -> Refusal {
    Refusal::Refused(fault.words())
}

fn peer(conversation: &str) -> Result<Peer, Fault> {
    Peer::parse(conversation).ok_or_else(|| Fault::Refused("That conversation isn't one Signal knows.".to_owned()))
}

/// A message id this adapter made: when it was sent, and by whom.
fn target(message_id: &str) -> Result<(u64, String), Fault> {
    wire::parse_message_id(message_id).ok_or_else(|| Fault::Refused("That message is no longer there.".to_owned()))
}

/// The words a quote quotes of a stored message: without the note that the rest is on the
/// phone, which is this device's invention for drawing the message and never travels.
fn quoted_words(text: &str) -> String {
    text.strip_suffix(wire::CONTINUES).map_or_else(|| text.to_owned(), str::to_owned)
}

fn quote_of(account: &Account, conversation: &str, reply_to: Option<&str>) -> Result<Option<Quote>, Fault> {
    Ok(match reply_to.map(target).transpose()? {
        Some((sent, author)) => {
            let stored = account.ctx.stored_message(conversation, &wire::message_id(sent, &author)).ok().flatten();
            let (text, spans) = stored.map_or_else(|| (String::new(), Vec::new()), |message| (quoted_words(&message.text), message.spans));
            Some(Quote { sent, author, text, spans, attachments: Vec::new() })
        }
        None => None,
    })
}

/// Formats outgoing text for Signal: GroupsV2 encode mentions as `\u{FFFC}` + UTF-16 `MentionAci`
/// ranges, while 1:1 DMs expand any `<@aci>` tokens into plain `@Name` text so official clients
/// never display a literal object-replacement glyph (`￼`).
fn format_outgoing_body(
    account: &Account,
    to: &Peer,
    text: &str,
    mention_spans: &[Span],
) -> (String, Vec<style::Range>, style::Styled) {
    let resolve = |aci: &str| {
        if aci == account.me {
            "you".to_owned()
        } else {
            account.live().known_name(aci)
        }
    };
    let (wire_body, ranges) = match to {
        Peer::Group(_) => style::parse_with_inline_mentions(text, mention_spans, resolve),
        Peer::Direct(_) => style::parse_plain_inline_mentions(text, resolve),
    };
    let shown = style::apply(&wire_body, &ranges, &account.me);
    (wire_body, ranges, shown)
}

fn send_text(
    account: &Account,
    conversation: &str,
    text: &str,
    reply_to: Option<&str>,
    mention_spans: &[Span],
    suppress_embeds: bool,
) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let (wire_body, ranges, shown) = format_outgoing_body(account, &to, text, mention_spans);
    let quote = quote_of(account, conversation, reply_to)?;
    let (timer, previews_enabled) = {
        let live = account.live();
        (live.timer_of(conversation), live.link_previews)
    };
    let mut previews = Vec::new();
    if previews_enabled && !suppress_embeds {
        if let Some(url) = crate::adapters::tiny::preview::first_https_url(&wire_body) {
            if let Ok(Some(fetched)) = account.wire.fetch_link_preview(&url) {
                if wire::preview_url_in_body(&shown.text, &fetched.url) {
                    previews.push(fetched);
                }
            }
        }
    }
    let sent = account.wire.send(&to, Body::Text {
        text: wire_body,
        quote: quote.clone(),
        ranges,
        timer,
        previews: previews.clone(),
    })?;
    account.live().sent(sent, conversation);
    let words = Text {
        author: account.me.clone(),
        name: account.name.clone(),
        sent,
        text: shown.text,
        spans: shown.spans,
        quote,
        previews,
        ..Text::default()
    };
    let mut message = message(account, conversation, &words, true);
    if timer.seconds > 0 {
        message.expires_at = Some(crate::hub::now() + i64::from(timer.seconds));
    }
    if !account.live().read_receipts {
        message.receipt_note = RECEIPTS_OFF.to_owned();
    }
    // A hard error fails the whole send above. A scripted wire can also name one person it missed.
    for person in account.wire.missed() {
        if message.deliveries.len() >= protocol::DELIVERIES_LIMIT {
            break;
        }
        let name = account.live().name(&person);
        message.deliveries.push(Delivery {
            person,
            name: if name == NO_NAME { String::new() } else { name },
            delivered: None,
            read: None,
            viewed: None,
            failed: "Signal could not deliver this.".to_owned(),
        });
    }
    Ok(message)
}

/// Files go on the upload thread: each is read in parts, then handed to Signal as one message.
fn send_with(account: &Arc<Account>, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: Outgoing) {
    if let Some(why) = files::refused(&with) {
        account.ctx.sent(conversation, client_id, Err(why));
        return;
    }
    if with.files.is_empty() {
        if let Some(pick) = with.pick {
            let outcome = match pick.kind {
                CatalogKind::Gifs => send_gif(account, conversation, client_id, text, reply_to, &pick.id).map_err(|fault| fault.words()),
                CatalogKind::Stickers => send_sticker(account, conversation, client_id, reply_to, &pick.id).map_err(|fault| fault.words()),
                _ => Err("Signal can't send that item from here.".to_owned()),
            };
            account.ctx.sent(conversation, client_id, outcome);
            return;
        }
        let outcome = send_text(account, conversation, text, reply_to, &with.spans, with.suppress_embeds).map_err(|fault| fault.words());
        account.ctx.sent(conversation, client_id, outcome);
        return;
    }
    let account = Arc::clone(account);
    let conversation = conversation.to_owned();
    let text = text.to_owned();
    let reply_to = reply_to.map(str::to_owned);
    let client_id_owned = client_id.to_owned();
    let voice = with.voice;
    let ctx = account.ctx.clone();
    let conversation_id = conversation.clone();
    upload::spawn(&ctx, &conversation_id, client_id, &with.files, move |upload| {
        send_files(&account, &conversation, &client_id_owned, &text, reply_to.as_deref(), voice, upload)
    });
}

fn send_files(
    account: &Account,
    conversation: &str,
    client_id: &str,
    text: &str,
    reply_to: Option<&str>,
    voice: Option<protocol::VoiceNote>,
    upload: &mut Upload,
) -> Result<Message, String> {
    if upload.files().len() > files::MAX_FILES as usize {
        return Err(format!("Signal takes at most {} files in one message.", files::MAX_FILES));
    }
    let count = upload.files().len();
    let mut prepared = Vec::with_capacity(count);
    for at in 0..count {
        let name = upload.files()[at].name.clone();
        let mime = upload.files()[at].mime.to_owned();
        let keep = upload.files()[at].as_file || upload.files()[at].original || voice.is_some();
        let len = upload.files()[at].bytes;
        if let Some(why) = files::too_big(&name, len) {
            return Err(why);
        }
        let mut bytes = Vec::new();
        upload.parts(at, 1024 * 1024, 0, |part| {
            bytes.extend_from_slice(part.bytes);
            Ok(())
        })?;
        let mut file = files::prepare(&name, &mime, bytes, keep);
        if voice.is_some() && at + 1 == count {
            file.voice = true;
            if !file.mime.starts_with("audio/") {
                file.mime = "audio/ogg".to_owned();
            }
        }
        prepared.push(file);
    }
    upload.check()?;
    let to = peer(conversation).map_err(|fault| fault.words())?;
    let quote = quote_of(account, conversation, reply_to).map_err(|fault| fault.words())?;
    // Write each file to disk first, then move that one copy to Signal. Cloning `prepared`
    // would keep every payload in memory a second time for the whole upload.
    let mut attachments: Vec<_> = prepared.iter().enumerate().map(|(index, file)| show_file(account, client_id, index, file)).collect();
    if let (Some(note), Some(att)) = (voice, attachments.last_mut()) {
        att.file.kind = MediaKind::Audio;
        att.duration = ((note.duration + 999) / 1_000).max(1);
        att.waveform = if note.waveform.is_empty() { vec![24; 32] } else { note.waveform };
    }
    let timer = account.live().timer_of(conversation);
    let (wire_body, ranges, shown) = format_outgoing_body(account, &to, text, &[]);
    let sent = match account.wire.send(&to, Body::Files { text: wire_body, quote: quote.clone(), ranges, files: prepared, timer }) {
        Ok(sent) => sent,
        Err(fault) => {
            forget_landed(&attachments);
            return Err(fault.words());
        }
    };
    account.live().echoed(sent);
    account.live().sent(sent, conversation);
    let words = Text {
        author: account.me.clone(),
        name: account.name.clone(),
        sent,
        text: shown.text,
        spans: shown.spans,
        quote,
        ..Text::default()
    };
    let mut message = message(account, conversation, &words, true);
    if timer.seconds > 0 {
        message.expires_at = Some(crate::hub::now() + i64::from(timer.seconds));
    }
    message.attachments = attachments;
    Ok(message)
}

/// The file as the row shows it, kept here so it stays after the staged copy is removed.
/// The id is the shell's client id, known before Signal names the message, so the bytes can
/// be written once and then moved to the wire.
fn show_file(account: &Account, client_id: &str, index: usize, file: &wire::File) -> Attachment {
    let media_kind = if file.gif { MediaKind::Gif } else { files::kind(&file.mime) };
    let mut media = MediaFile::new(format!("signal:{client_id}:{index}"), media_kind);
    media.width = file.width;
    media.height = file.height;
    media.bytes = file.bytes.len() as u64;
    if let Some(landed) = account.ctx.landed(&media, &format!("signal://file/{client_id}/{index}"), &file.bytes) {
        media = landed;
    }
    let mut attachment = Attachment::new(file.name.clone(), media);
    attachment.mime = file.mime.clone();
    if !file.thumb.is_empty() {
        let thumb = MediaFile::new(format!("signal:{client_id}:{index}:thumb"), MediaKind::Image);
        if let Some(landed) = account.ctx.landed(&thumb, &format!("signal://file/{client_id}/{index}/thumb"), &file.thumb) {
            attachment.thumb = Some(landed);
        }
    }
    attachment
}

/// A send that did not go must not leave its files on disk.
fn forget_landed(attachments: &[Attachment]) {
    for attachment in attachments {
        if let Some(path) = &attachment.file.path {
            let _ = std::fs::remove_file(path);
        }
        if let Some(thumb) = &attachment.thumb {
            if let Some(path) = &thumb.path {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn react(account: &Account, conversation: &str, message_id: &str, emoji: &str, on: bool) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let (sent, author) = target(message_id)?;
    // One reaction a person, on Signal: taking back sends the one being taken back.
    account.wire.send(&to, Body::React { author, target: sent, emoji: emoji.to_owned(), remove: !on })?;
    let replaced = account.live().react(message_id, &account.me, on.then_some(emoji));
    if let Some(old) = replaced.filter(|_| on) {
        let _ = account.ctx.reacted(conversation, message_id, &Reaction { emoji: old, file: None, count: 1, mine: true, who: vec![account.me.clone()], ..Reaction::default() }, false);
    }
    Ok(())
}

fn own(account: &Account, message_id: &str) -> Result<u64, Fault> {
    let (sent, author) = target(message_id)?;
    if author != account.me {
        return Err(Fault::Refused("Only your own messages can be changed.".to_owned()));
    }
    Ok(sent)
}

fn edit(account: &Account, conversation: &str, message_id: &str, text: &str, mention_spans: &[Span]) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let sent = own(account, message_id)?;
    if !can(sent, now_ms(), true, conversation == account.me).edit {
        return Err(Fault::Refused("This message can no longer be edited.".to_owned()));
    }
    let (wire_body, ranges, shown) = format_outgoing_body(account, &to, text, mention_spans);
    let timer = account.live().timer_of(conversation);
    account.wire.send(&to, Body::Edit { target: sent, text: wire_body, ranges, timer })?;
    if let Ok(Some(mut message)) = account.ctx.stored_message(conversation, message_id) {
        message.text = shown.text;
        message.spans = shown.spans;
        message.edited = true;
        let _ = account.ctx.message(conversation, &message);
    }
    Ok(())
}

/// The lab's signal-text-styles driver. A build without this keeps running.
pub fn prove_signal_styles() -> Result<(), String> {
    style::prove()
}

fn delete(account: &Account, conversation: &str, message_id: &str) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let (sent, author) = target(message_id)?;
    let offer = can(sent, now_ms(), author == account.me, conversation == account.me);
    // Past the window, other clients ignore a delete for everyone. Tell only this account's
    // devices, and remove it here either way.
    let body = if offer.delete_for_everyone { Body::Delete { target: sent } } else { Body::DeleteForMe { author, target: sent } };
    account.wire.send(&to, body)?;
    let _ = account.ctx.deleted(conversation, message_id);
    Ok(())
}

fn note_to_self(account: &Account, peer: &Peer) -> bool {
    matches!(peer, Peer::Direct(aci) if aci == &account.me)
}

fn start_expiry(account: &Account, conversation: &str, message_id: &str, seconds: u32) {
    if seconds == 0 {
        return;
    }
    if let Ok(Some(mut stored)) = account.ctx.stored_message(conversation, message_id) {
        if stored.expires_at.is_none() {
            stored.expires_at = Some(crate::hub::now() + i64::from(seconds));
            let _ = account.ctx.message(conversation, &stored);
        }
    }
}

/// The user read up to a message: the user's other devices are told, and the authors too when
/// the user's Signal settings send read receipts.
fn mark_read(account: &Account, conversation: &str, message_id: &str) {
    // Message requests from strangers send no read receipts until accepted.
    if account.live().requests.contains(conversation) {
        return;
    }
    let Ok((upto, author)) = target(message_id) else {
        return;
    };
    let (mut read, receipts, mut to_expire) = {
        let mut live = account.live();
        let read = live.read_upto(conversation, upto);
        let mut to_expire = Vec::new();
        for (a, s) in &read {
            let mid = wire::message_id(*s, a);
            if let Some(secs) = live.take_expiring(conversation, &mid) {
                to_expire.push((mid, secs));
            }
        }
        (read, live.read_receipts, to_expire)
    };
    // Unread from before chatd started is not remembered: the message read stands for it.
    if read.is_empty() && author != account.me {
        let mid = wire::message_id(upto, &author);
        if let Some(secs) = account.live().take_expiring(conversation, &mid) {
            to_expire.push((mid, secs));
        }
        read.push((author, upto));
    }
    for (mid, secs) in to_expire {
        start_expiry(account, conversation, &mid, secs);
    }
    if read.is_empty() {
        return;
    }
    if let Err(fault) = account.wire.read(&read, receipts) {
        tracing::warn!("chatd: Signal: read: {}", super::net::redact(&fault.to_string()));
    }
}

/// The user played a voice message or viewed a story: syncs to other linked devices and sends a
/// `Viewed` receipt to the author when read receipts are enabled on both the Signal account and
/// this device.
fn viewed(account: &Account, conversation: &str, message_id: &str) {
    if account.live().requests.contains(conversation) {
        return;
    }
    let Ok((sent, author)) = target(message_id) else {
        return;
    };
    if author == account.me {
        return;
    }
    let receipts = account.live().read_receipts && account.ctx.hub.settings().receipts(&account.ctx.account);
    if let Err(fault) = account.wire.viewed(&[(author, sent)], receipts) {
        tracing::warn!("chatd: Signal: viewed: {}", super::net::redact(&fault.to_string()));
    }
}

/// What a `Request::Poll` asked for, once the sentinel is read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollAct {
    /// Replace our vote. Empty takes it back.
    Vote(Vec<u32>),
    /// End the poll. Only its author may.
    End,
}

/// Packs a poll request into the edit text the Signal command thread already understands.
pub fn poll_command(votes: Option<&[u32]>) -> String {
    match votes {
        None => POLL_END.to_owned(),
        Some(votes) => {
            let list = votes.iter().take(POLL_MOST).map(|index| index.to_string()).collect::<Vec<_>>().join(",");
            format!("{POLL_VOTE}{list}")
        }
    }
}

/// The other way. `None` is an ordinary edit.
pub fn poll_action(text: &str) -> Option<PollAct> {
    if text == POLL_END {
        return Some(PollAct::End);
    }
    let rest = text.strip_prefix(POLL_VOTE)?;
    if rest.is_empty() {
        return Some(PollAct::Vote(Vec::new()));
    }
    let mut votes = Vec::new();
    for part in rest.split(',') {
        votes.push(part.parse().ok()?);
    }
    Some(PollAct::Vote(votes))
}

/// What `TINYCLIENT_PROVE=signal-poll` checks, with no account and no network: a vote, a
/// retraction and an end pack and unpack, and ordinary text does not.
pub fn prove_signal_poll() -> Result<(), String> {
    match poll_action(&poll_command(Some(&[0, 2]))) {
        Some(PollAct::Vote(indexes)) if indexes == [0, 2] => {}
        _ => return Err("a vote did not round-trip".into()),
    }
    match poll_action(&poll_command(Some(&[]))) {
        Some(PollAct::Vote(indexes)) if indexes.is_empty() => {}
        _ => return Err("taking a vote back did not round-trip".into()),
    }
    match poll_action(&poll_command(None)) {
        Some(PollAct::End) => {}
        _ => return Err("ending a poll did not round-trip".into()),
    }
    if poll_action("Dinner?").is_some() {
        return Err("plain text was read as a poll".into());
    }
    Ok(())
}

fn checked_draft(draft: &PollDraft) -> Result<(String, Vec<String>, bool), Fault> {
    let question = draft.question.trim();
    if question.is_empty() {
        return Err(Fault::Refused("A poll needs a question.".into()));
    }
    if question.chars().count() > POLL_CHARS {
        return Err(Fault::Refused("A poll's question can be at most 100 characters.".into()));
    }
    let options: Vec<String> = draft.options.iter().map(|option| option.trim().to_owned()).filter(|option| !option.is_empty()).collect();
    if options.len() < 2 {
        return Err(Fault::Refused("A poll needs at least two choices.".into()));
    }
    if options.len() > POLL_MOST {
        return Err(Fault::Refused("A poll can have at most 10 choices.".into()));
    }
    if options.iter().any(|option| option.chars().count() > POLL_CHARS) {
        return Err(Fault::Refused("A poll's choice can be at most 100 characters.".into()));
    }
    Ok((question.to_owned(), options, draft.multiple))
}

fn with_poll(account: &Account, conversation: &str, author: &str, name: &str, sent: u64, outgoing: bool, mut poll: Poll) -> Message {
    protocol::clip(&mut poll.question, POLL_CHARS);
    let words = Text { author: author.to_owned(), name: name.to_owned(), sent, text: format!("Poll: {}", poll.question), ..Text::default() };
    let mut message = message(account, conversation, &words, outgoing);
    message.poll = Some(Box::new(poll));
    message
}

/// Records one person's vote, replacing their earlier one when `serial` is newer. `serial` of 0
/// means this vote follows the one we already have.
fn apply_ballot(poll: &mut Poll, person: &str, mut choices: Vec<u32>, serial: u32, me: &str) -> bool {
    if poll.closed {
        return false;
    }
    choices.retain(|index| (*index as usize) < poll.choices.len());
    choices.sort_unstable();
    choices.dedup();
    if !poll.multiple {
        choices.truncate(1);
    }
    let serial = if serial == 0 { poll.ballots.iter().find(|ballot| ballot.person == person).map(|ballot| ballot.serial.saturating_add(1)).unwrap_or(1) } else { serial };
    if let Some(existing) = poll.ballots.iter().find(|ballot| ballot.person == person) {
        if serial <= existing.serial {
            return false;
        }
    }
    if poll.ballots.len() >= BALLOTS_KEPT && !poll.ballots.iter().any(|ballot| ballot.person == person) {
        poll.ballots.remove(0);
    }
    poll.ballots.retain(|ballot| ballot.person != person);
    poll.ballots.push(PollBallot { person: person.to_owned(), choices, serial });
    recount(poll, me);
    true
}

fn recount(poll: &mut Poll, me: &str) {
    let ballots = poll.ballots.clone();
    for choice in &mut poll.choices {
        choice.voters.clear();
        choice.count = 0;
    }
    for ballot in &ballots {
        for index in &ballot.choices {
            if let Some(choice) = poll.choices.get_mut(*index as usize) {
                choice.count = choice.count.saturating_add(1);
                if choice.voters.len() < protocol::POLL_VOTERS {
                    choice.voters.push(ballot.person.clone());
                }
            }
        }
    }
    poll.mine = ballots.into_iter().find(|ballot| ballot.person == me).map(|ballot| ballot.choices).unwrap_or_default();
}

fn chosen(poll: &Poll, indexes: &[u32]) -> Result<Vec<u32>, Fault> {
    if indexes.is_empty() {
        return Ok(Vec::new());
    }
    if indexes.iter().any(|index| *index as usize >= poll.choices.len()) {
        return Err(Fault::Refused("That choice is not on this poll.".into()));
    }
    let mut indexes = indexes.to_vec();
    indexes.sort_unstable();
    indexes.dedup();
    if !poll.multiple && indexes.len() > 1 {
        return Err(Fault::Refused("This poll takes one choice.".into()));
    }
    Ok(indexes)
}

fn send_contact(account: &Account, conversation: &str, contact: protocol::SharedContact) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    if contact.name.trim().is_empty() && contact.phones.is_empty() && contact.emails.is_empty() {
        return Err(Fault::Refused("A shared contact needs a name, phone number, or email.".to_owned()));
    }
    let timer = account.live().timer_of(conversation);
    let sent = account.wire.send(&to, Body::Contact { contact: contact.clone(), timer })?;
    account.live().sent(sent, conversation);
    let words = Text { author: account.me.clone(), name: account.name.clone(), sent, contacts: vec![contact], ..Text::default() };
    let mut message = message(account, conversation, &words, true);
    if timer.seconds > 0 {
        message.expires_at = Some(crate::hub::now() + i64::from(timer.seconds));
    }
    Ok(message)
}

fn send_poll(account: &Account, conversation: &str, draft: &PollDraft, _reply_to: Option<&str>) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let (question, options, multiple) = checked_draft(draft)?;
    let timer = account.live().timer_of(conversation);
    let sent = account.wire.send(&to, Body::Poll { question: question.clone(), options: options.clone(), multiple, timer })?;
    account.live().sent(sent, conversation);
    let mut message = with_poll(account, conversation, &account.me, &account.name, sent, true, Poll::open(question, options, multiple));
    if timer.seconds > 0 {
        message.expires_at = Some(crate::hub::now() + i64::from(timer.seconds));
    }
    Ok(message)
}

fn poll_act(account: &Account, conversation: &str, message_id: &str, act: PollAct) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let (sent, author) = target(message_id)?;
    let Some(mut message) = account.ctx.stored_message(conversation, message_id).ok().flatten() else {
        return Err(Fault::Refused("That message is no longer there.".into()));
    };
    let Some(poll) = message.poll.as_ref() else {
        return Err(Fault::Refused("That is not a poll.".into()));
    };
    match act {
        PollAct::End => {
            if author != account.me {
                return Err(Fault::Refused("Only the person who started this poll can end it.".into()));
            }
            if poll.closed {
                return Ok(());
            }
            account.wire.send(&to, Body::PollEnd { target: sent })?;
            if let Some(poll) = message.poll.as_mut() {
                poll.closed = true;
            }
            let _ = account.ctx.message(conversation, &message);
            Ok(())
        }
        PollAct::Vote(indexes) => {
            if poll.closed {
                return Err(Fault::Refused("This poll has ended.".into()));
            }
            let choices = chosen(poll, &indexes)?;
            let serial = poll.ballots.iter().find(|ballot| ballot.person == account.me).map(|ballot| ballot.serial.saturating_add(1)).unwrap_or(1);
            account.wire.send(&to, Body::PollVote { author, target: sent, indexes: choices.clone(), serial })?;
            if let Some(poll) = message.poll.as_mut() {
                apply_ballot(poll, &account.me, choices, serial, &account.me);
            }
            let _ = account.ctx.message(conversation, &message);
            Ok(())
        }
    }
}

fn typing(account: &Account, conversation: &str) {
    let Some(to) = Peer::parse(conversation) else {
        return;
    };
    {
        let mut live = account.live();
        if live.requests.contains(conversation)
            || !live.typing
            || live.typed.get(conversation).is_some_and(|told| told.elapsed() < TYPING_EVERY)
        {
            return;
        }
        live.typed.retain(|_, told| told.elapsed() < TYPING_EVERY);
        live.typed.insert(conversation.to_owned(), Instant::now());
    }
    let _ = account.wire.typing(&to, true);
}

/// A story still on this device: held, unexpired, and not from a blocked author or group.
fn visible_story(account: &Account, id: &str) -> Option<HeldStory> {
    let live = account.live();
    let story = live.stories.get(id)?.clone();
    if story.expires_at <= crate::hub::now() || live.blocked.contains(&story.author) {
        return None;
    }
    if let Some(gid) = &story.group_id {
        if live.blocked.contains(&Peer::Group(gid.clone()).conversation()) {
            return None;
        }
    }
    Some(story)
}

/// `Some(true)` when `by` may have seen our story sent at `sent_at`.
/// `Some(false)` when that story is held but `by` is not in its audience or its group.
/// `None` when this time is not one of our held stories (a voice note, for example).
fn story_view_from(account: &Account, by: &str, sent_at: u64) -> Option<bool> {
    let id = wire::message_id(sent_at, &account.me);
    let live = account.live();
    let story = live.stories.get(&id)?;
    if story.expires_at <= crate::hub::now() || by.is_empty() || by == account.me {
        return Some(false);
    }
    if let Some(gid) = &story.group_id {
        let conv = Peer::Group(gid.clone()).conversation();
        if live.blocked.contains(&conv) {
            return Some(false);
        }
        return Some(live.groups.get(&conv).is_some_and(|members| members.iter().any(|id| id == by)));
    }
    Some(story.audience.iter().any(|aci| aci == by))
}

fn note_story_viewer(account: &Account, sent_at: u64, by: &str) {
    let id = wire::message_id(sent_at, &account.me);
    let mut live = account.live();
    if let Some(story) = live.stories.get_mut(&id) {
        if !story.viewed_by.iter().any(|seen| seen == by) {
            story.viewed_by.push(by.to_owned());
        }
    }
}

/// One receipt for one of our messages. A direct chat's tick moves on that one receipt. A
/// group's tick moves only when every other member has reached that state. A receipt whose
/// message is no longer among the ones remembered, and is not in the store, is dropped: it is
/// not filed onto the reader's own chat.
fn file_receipt(account: &Account, by: &str, sent_at: u64, read: bool, viewed: bool, at: i64) {
    if by.is_empty() || by == account.me {
        return;
    }
    let id = wire::message_id(sent_at, &account.me);
    let remembered = {
        let live = account.live();
        live.sent.iter().rev().find(|(when, _)| *when == sent_at).map(|(_, conversation)| conversation.clone())
    };
    let conversation = match remembered {
        Some(conversation) => conversation,
        None => match account.ctx.conversation_of_message(&id) {
            Ok(Some(conversation)) => conversation,
            _ => return,
        },
    };
    let Ok(Some(mut message)) = account.ctx.stored_message(&conversation, &id) else {
        return;
    };
    let (delivered, seen) = {
        let live = account.live();
        note_receipt(&mut message, by, &live.name(by), read, viewed, at, live.read_receipts);
        let members = live.groups.get(&conversation).map(Vec::as_slice);
        let others = others_of(members, &conversation, &account.me);
        ticks(&message, others, members, &account.me)
    };
    let _ = account.ctx.message(&conversation, &message);
    if delivered {
        let _ = account.ctx.delivered(&conversation, &id);
    }
    if seen {
        let _ = account.ctx.seen(&conversation, &id);
    }
}

/// How many other people a tick waits for. A direct chat is one. A group with no member list
/// does not move: one receipt must not speak for a group we cannot count.
fn others_of(members: Option<&[String]>, conversation: &str, me: &str) -> usize {
    match members {
        Some(ids) => {
            let count = ids.iter().filter(|id| id.as_str() != me && !id.is_empty()).count();
            if count == 0 { usize::MAX } else { count }
        }
        None if conversation.starts_with("g.") => usize::MAX,
        None => 1,
    }
}

/// `(delivered, seen)` once `others` distinct members, not us, have reached that state.
fn ticks(message: &Message, others: usize, members: Option<&[String]>, me: &str) -> (bool, bool) {
    let count = |read: bool| -> usize {
        message
            .deliveries
            .iter()
            .filter(|delivery| {
                delivery.person != me
                    && delivery.failed.is_empty()
                    && members.is_none_or(|ids| ids.iter().any(|id| id == &delivery.person))
                    && if read {
                        delivery.read.is_some() || delivery.viewed.is_some()
                    } else {
                        delivery.delivered.is_some() || delivery.read.is_some() || delivery.viewed.is_some()
                    }
            })
            .count()
    };
    (others > 0 && count(false) >= others, others > 0 && count(true) >= others)
}

fn note_receipt(message: &mut Message, person: &str, name: &str, read: bool, viewed: bool, at: i64, receipts_on: bool) {
    if let Some(delivery) = entry(&mut message.deliveries, person, name) {
        if at > 0 {
            stamp(&mut delivery.delivered, at);
            if read || viewed {
                stamp(&mut delivery.read, at);
            }
            if viewed {
                stamp(&mut delivery.viewed, at);
            }
        }
        if delivery.delivered.is_some() {
            delivery.failed.clear();
        }
    }
    if receipts_on {
        if message.receipt_note == RECEIPTS_OFF {
            message.receipt_note.clear();
        }
    } else {
        message.receipt_note = RECEIPTS_OFF.to_owned();
    }
}

fn entry<'a>(deliveries: &'a mut Vec<Delivery>, person: &str, name: &str) -> Option<&'a mut Delivery> {
    if let Some(at) = deliveries.iter().position(|delivery| delivery.person == person) {
        if deliveries[at].name.is_empty() && !name.is_empty() && name != NO_NAME {
            deliveries[at].name = name.to_owned();
        }
        return Some(&mut deliveries[at]);
    }
    if deliveries.len() >= protocol::DELIVERIES_LIMIT {
        return None;
    }
    deliveries.push(Delivery {
        person: person.to_owned(),
        name: if name.is_empty() || name == NO_NAME { String::new() } else { name.to_owned() },
        delivered: None,
        read: None,
        viewed: None,
        failed: String::new(),
    });
    deliveries.last_mut()
}

fn stamp(slot: &mut Option<i64>, at: i64) {
    if slot.is_none_or(|have| have < at) {
        *slot = Some(at);
    }
}

/// The lab's check: two readers, and the group is seen only once both have read. A played
/// message counts as read. With read receipts off, the message says so.
pub fn prove_signal_message_info() -> Result<(), String> {
    let mut message = Message::plain("1", "Ada", "Deal?", 1, true);
    note_receipt(&mut message, "grace", "Grace", false, false, 10, true);
    note_receipt(&mut message, "alan", "Alan", false, false, 11, true);
    let (delivered, seen) = ticks(&message, 2, None, "me");
    if !delivered || seen {
        return Err("two deliveries were not delivered, or were already seen".into());
    }
    note_receipt(&mut message, "grace", "Grace", true, false, 12, true);
    if ticks(&message, 2, None, "me").1 {
        return Err("one reader marked the group seen".into());
    }
    note_receipt(&mut message, "alan", "Alan", true, true, 13, true);
    if !ticks(&message, 2, None, "me").1 {
        return Err("both readers did not mark the group seen".into());
    }
    let alan = message.deliveries.iter().find(|delivery| delivery.person == "alan").ok_or("Alan is missing")?;
    if alan.viewed.is_none() || alan.read.is_none() {
        return Err("a played message was not read".into());
    }
    let mut quiet = Message::plain("2", "Ada", "Quiet", 1, true);
    note_receipt(&mut quiet, "grace", "Grace", true, false, 14, false);
    if quiet.receipt_note != RECEIPTS_OFF {
        return Err("read receipts off said nothing".into());
    }
    Ok(())
}

/// What the message menu may offer. `sent_ms` and `now_ms` are milliseconds. `own` is a message
/// this account sent. `note_to_self` is the chat with oneself, which has no delete-for-everyone limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub edit: bool,
    /// Delete for everyone. Note to Self has no time limit; every other chat has one day.
    pub delete_for_everyone: bool,
    /// Always. This removes the message on this account's devices only.
    pub delete_for_me: bool,
}

/// Signal Desktop sends an edit or a delete for everyone for one day. Delete for me is always offered.
pub fn can(sent_ms: u64, now_ms: u64, own: bool, note_to_self: bool) -> Offer {
    let within_day = now_ms.saturating_sub(sent_ms) <= SEND_WINDOW_MS;
    Offer { edit: own && within_day, delete_for_everyone: own && (note_to_self || within_day), delete_for_me: true }
}

/// An incoming edit or delete for everyone is kept for two days, and without a limit in Note to Self.
pub fn accept_remote(sent_ms: u64, now_ms: u64, note_to_self: bool) -> bool {
    note_to_self || now_ms.saturating_sub(sent_ms) <= RECEIVE_WINDOW_MS
}

/// The lab driver for `signal-delete-for-me`. A build that offers a late delete for everyone fails.
pub fn prove_signal_delete_for_me() -> Result<(), String> {
    let hour = 60 * 60 * 1000;
    let now = 50 * hour;
    let recent = can(now - hour, now, true, false);
    if !recent.edit || !recent.delete_for_everyone || !recent.delete_for_me {
        return Err("a recent message must offer an edit and both deletes".into());
    }
    let late = can(now - 25 * hour, now, true, false);
    if late.edit || late.delete_for_everyone || !late.delete_for_me {
        return Err("a day-old message must offer only delete for me".into());
    }
    let note = can(now - 25 * hour, now, true, true);
    if !note.delete_for_everyone || note.edit {
        return Err("note to self keeps delete for everyone and still closes the edit window".into());
    }
    let theirs = can(now - hour, now, false, false);
    if theirs.edit || theirs.delete_for_everyone || !theirs.delete_for_me {
        return Err("someone else's message offers only delete for me".into());
    }
    if !accept_remote(now - 47 * hour, now, false) || accept_remote(now - 49 * hour, now, false) {
        return Err("an incoming delete for everyone is kept for two days".into());
    }
    if !accept_remote(now - 49 * hour, now, true) {
        return Err("note to self keeps an old delete for everyone".into());
    }
    Ok(())
}

/// The lab's signal-send-file driver. A build without the hook stays up, so the driver fails there.
pub fn prove_signal_send_file() -> Result<(), String> {
    let can = files::capabilities();
    if can.max_file_bytes != files::MAX_FILE_BYTES || can.max_files != files::MAX_FILES || can.caption_limit != 0 {
        return Err("files are not offered".into());
    }
    if files::blurhash(&[(255, 0, 0)], 1, 1, 1, 1) != "00TI:j" {
        return Err("a solid red picture did not hash".into());
    }
    if !files::replace_outgoing_sync(true) || files::replace_outgoing_sync(false) {
        return Err("a phone copy would replace the file".into());
    }
    let line = files::call_line("b2b2b2b2-0000-4000-8000-000000000002", &["card.png".into()], "the hand", None);
    if line != "send b2b2b2b2-0000-4000-8000-000000000002 files card.png the hand" {
        return Err("the send was not recorded as a file".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_vote_is_not_an_edit() {
        prove_signal_poll().unwrap();
    }

    #[test]
    fn a_session_names_one_account_and_says_nothing_of_its_key() {
        let session = r#"{"aci":"0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b","name":"Ada","vault":"00ff"}"#;
        let found = accounts(session).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].account.as_str(), found[0].name.as_str()), ("0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b", "Ada"));
        assert!(accounts(r#"{"aci":"../etc"}"#).is_err());
        assert_eq!(without(session, &found[0].account), None);
        assert!(!protocol::redact(session).contains("00ff"));
    }

    #[test]
    fn a_new_reaction_from_the_same_person_replaces_theirs() {
        let mut live = Live::default();
        assert_eq!(live.react("m", "p", Some("👍")), None);
        assert_eq!(live.react("m", "p", Some("❤️")), Some("👍".to_owned()));
        assert_eq!(live.react("m", "p", Some("❤️")), None);
        assert_eq!(live.react("m", "p", None), Some("❤️".to_owned()));
    }

    #[test]
    fn reaction_record_bounds_size_and_skips_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acct.reactions");

        // Pre-populate more than REACTIONS_KEPT rows; loading + writing prunes to REACTIONS_KEPT newest.
        let rows: Vec<KeptReaction> = (0..=REACTIONS_KEPT)
            .map(|i| KeptReaction { message: format!("m{i}"), person: "p".into(), emoji: "👍".into() })
            .collect();
        std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();

        let mut live = Live { reactions_path: path.clone(), ..Live::default() };
        assert_eq!(live.react("m0", "p", Some("❤️")), None, "oldest row beyond cap was evicted");
        let kept = read_reactions(&path);
        assert_eq!(kept.len(), REACTIONS_KEPT);
        assert_eq!(kept.last().map(|r| (r.message.as_str(), r.emoji.as_str())), Some(("m0", "❤️")));
        assert!(!path.with_extension("tmp").exists(), "temp file was renamed away");

        // An oversized file is skipped on load rather than read into memory.
        let oversized = vec![b' '; (REACTIONS_FILE_LIMIT as usize) + 1];
        std::fs::write(&path, oversized).unwrap();
        assert!(read_reactions(&path).is_empty());
    }

    #[test]
    fn a_username_shape_is_proven() {
        prove_signal_username().unwrap();
    }

    #[test]
    fn reading_takes_what_is_older_and_leaves_what_is_newer() {
        let mut live = Live::default();
        for sent in [1, 2, 3] {
            live.unread("c", "a", sent);
        }
        assert_eq!(live.read_upto("c", 2), vec![("a".to_owned(), 1), ("a".to_owned(), 2)]);
        assert_eq!(live.unread["c"].len(), 1);
        assert_eq!(live.read_upto("c", 3).len(), 1);
        assert!(!live.unread.contains_key("c"));
    }

    #[test]
    fn a_profile_edit_is_not_a_status() {
        let packed = profile_status(Some("Ada"), Some("New about"), Some(""));
        let (name, about, emoji) = profile_edit(&packed).unwrap();
        assert_eq!((name.as_deref(), about.as_deref(), emoji.as_deref()), (Some("Ada"), Some("New about"), Some("")));
        assert!(profile_edit("Lunch").is_none());
        assert!(prove_signal_own_profile().is_ok());
    }

    #[test]
    fn a_quote_quotes_the_words_without_the_note() {
        let body = "y".repeat(2_048);
        assert_eq!(quoted_words(&format!("{body}{}", wire::CONTINUES)), body);
        assert_eq!(quoted_words("Tuesday, seven sharp."), "Tuesday, seven sharp.");
    }

    #[test]
    fn group_revisions_gate_connect_stale_duplicate_and_newer() {
        let mut revisions = GroupRevisions::default();
        let initial = GroupSnap { title: "Bridge club".into(), ..GroupSnap::default() };
        assert!(!revisions.contains("g1"));
        assert!(!revisions.should_check("g1", 1));
        assert_eq!(revisions.advance("g1", 1, initial.clone()), None);

        // First snapshot is seeded silently and not overwritten by a second seed.
        revisions.seed("g1".into(), 5, initial.clone());
        assert!(revisions.contains("g1"));
        let renamed = GroupSnap { title: "Wednesday club".into(), ..GroupSnap::default() };
        revisions.seed("g1".into(), 6, renamed.clone());

        // Stale and duplicate revisions are skipped.
        assert!(!revisions.should_check("g1", 4));
        assert!(!revisions.should_check("g1", 5));
        assert_eq!(revisions.advance("g1", 4, renamed.clone()), None);
        assert_eq!(revisions.advance("g1", 5, renamed.clone()), None);

        // A newer revision is announced once, then skipped on repeat.
        assert!(revisions.should_check("g1", 6));
        let before = revisions.advance("g1", 6, renamed.clone()).expect("newer revision");
        assert_eq!(before, initial);
        assert!(!revisions.should_check("g1", 6));
        assert_eq!(revisions.advance("g1", 6, renamed), None);
    }

    #[test]
    fn rejoining_a_group_sets_rejoined_and_clears_left() {
        let out = GroupSnap::default();
        let inside = GroupSnap { members: vec![MemberSnap { id: "me".into(), admin: false }], ..GroupSnap::default() };
        let left = group_delta(&inside, &out, "me", |_| String::new());
        assert!(left.left && !left.rejoined);
        let back = group_delta(&out, &inside, "me", |_| String::new());
        assert!(!back.left && back.rejoined);
        assert_eq!(back.lines, ["You joined the group"]);
    }

    #[test]
    fn a_group_is_seen_only_when_every_other_member_has_read() {
        super::prove_signal_message_info().unwrap();
    }
}
