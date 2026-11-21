//! The seam between the adapter and the Signal library. Everything above it speaks the contract;
//! everything below it is Signal: keys, sessions, sealed sender, the socket. `real` is presage
//! (built with the `signal` feature); `fake` is a script, so the adapter can be run through
//! chatd's socket with no Signal behind it and no Signal library linked.
//!
//! Signal names a message by who wrote it and the millisecond they sent it, and has no list of
//! conversations or history on its servers: what arrives is all there is.

use super::style::Range;
use protocol::Span;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// Where a message goes or came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Peer {
    /// A person, by their account id (ACI).
    Direct(String),
    /// A group, by its public id in hex. The group's master key never leaves the wire.
    Group(String),
}

const GROUP: &str = "g.";

impl Peer {
    /// The conversation id the contract knows it by.
    pub fn conversation(&self) -> String {
        match self {
            Peer::Direct(aci) => aci.clone(),
            Peer::Group(id) => format!("{GROUP}{id}"),
        }
    }

    pub fn parse(conversation: &str) -> Option<Peer> {
        match conversation.strip_prefix(GROUP) {
            Some(id) if !id.is_empty() && id.bytes().all(|c| c.is_ascii_hexdigit()) => Some(Peer::Group(id.to_owned())),
            Some(_) => None,
            None if is_aci(conversation) => Some(Peer::Direct(conversation.to_owned())),
            None => None,
        }
    }
}

/// An account id as Signal writes one: a UUID in lower case.
pub fn is_aci(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, c)| if matches!(at, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() && !c.is_ascii_uppercase() })
}

/// The id of a message in the contract: when it was sent, in milliseconds, and who wrote it.
pub fn message_id(sent: u64, author: &str) -> String {
    format!("{sent}-{author}")
}

/// The other way: `None` for an id this adapter did not make.
pub fn parse_message_id(id: &str) -> Option<(u64, String)> {
    let (sent, author) = id.split_once('-')?;
    let sent = sent.parse().ok()?;
    is_aci(author).then(|| (sent, author.to_owned()))
}

/// Someone in the user's contacts, as the phone shares them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contact {
    pub id: String,
    pub name: String,
    /// In international form; empty when the phone did not share it.
    pub phone: String,
}

/// Who is permitted to perform a group action in Signal Groups v2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupAccess {
    #[default]
    Member,
    Administrator,
    Unsatisfiable,
    Any,
}

/// One member, pending invitee, or join requester of a Signal group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupMember {
    pub id: String,
    pub name: String,
    pub admin: bool,
}

/// Mode for a Signal group's invite link (`https://signal.group/#...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteLinkMode {
    Off,
    Enabled,
    ApprovalRequired,
    Reset,
}

/// Decoded contents of a `https://signal.group/#<base64>` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInviteLink {
    pub master_key: [u8; 32],
    pub password: Vec<u8>,
}

/// Encodes a `GroupMasterKey` (32 bytes) and `inviteLinkPassword` into a `https://signal.group/#...` URL
/// using Signal's `GroupInviteLink` protobuf wire format (`field 1: v1Contents { 1: masterKey, 2: password }`).
pub fn format_group_invite_link(master_key: &[u8; 32], password: &[u8]) -> String {
    use base64::Engine as _;
    fn push_varint(out: &mut Vec<u8>, mut val: usize) {
        while val >= 0x80 {
            out.push(((val & 0x7f) as u8) | 0x80);
            val >>= 7;
        }
        out.push(val as u8);
    }
    let mut inner = Vec::with_capacity(2 + 32 + 2 + password.len());
    inner.push((1 << 3) | 2);
    push_varint(&mut inner, 32);
    inner.extend_from_slice(master_key);
    if !password.is_empty() {
        inner.push((2 << 3) | 2);
        push_varint(&mut inner, password.len());
        inner.extend_from_slice(password);
    }
    let mut outer = Vec::with_capacity(2 + inner.len());
    outer.push((1 << 3) | 2);
    push_varint(&mut outer, inner.len());
    outer.extend_from_slice(&inner);
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&outer);
    format!("https://signal.group/#{encoded}")
}

/// Parses a `https://signal.group/#<base64>` URL into its 32-byte master key and invite link password.
pub fn parse_group_invite_link(url: &str) -> Option<GroupInviteLink> {
    use base64::Engine as _;
    fn read_varint(bytes: &[u8], at: &mut usize) -> Option<usize> {
        let mut out = 0usize;
        let mut shift = 0u32;
        loop {
            let b = *bytes.get(*at)?;
            *at += 1;
            out |= ((b & 0x7f) as usize) << shift;
            if b & 0x80 == 0 {
                return Some(out);
            }
            shift += 7;
            if shift > 35 {
                return None;
            }
        }
    }
    let trimmed = url.trim();
    let frag = trimmed
        .strip_prefix("https://signal.group/#")
        .or_else(|| trimmed.strip_prefix("http://signal.group/#"))?;
    if frag.is_empty() {
        return None;
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(frag)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(frag))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(frag))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(frag))
        .ok()?;
    let mut at = 0;
    let mut inner: Option<&[u8]> = None;
    while at < raw.len() {
        let tag = read_varint(&raw, &mut at)?;
        let field = tag >> 3;
        let wire = tag & 7;
        if wire != 2 {
            return None;
        }
        let len = read_varint(&raw, &mut at)?;
        let slice = raw.get(at..at + len)?;
        at += len;
        if field == 1 {
            inner = Some(slice);
        }
    }
    let inner = inner?;
    let mut iat = 0;
    let mut master_key: Option<[u8; 32]> = None;
    let mut password: Vec<u8> = Vec::new();
    while iat < inner.len() {
        let tag = read_varint(inner, &mut iat)?;
        let field = tag >> 3;
        let wire = tag & 7;
        if wire != 2 {
            return None;
        }
        let len = read_varint(inner, &mut iat)?;
        let slice = inner.get(iat..iat + len)?;
        iat += len;
        match field {
            1 => master_key = slice.try_into().ok(),
            2 => password = slice.to_vec(),
            _ => {}
        }
    }
    Some(GroupInviteLink { master_key: master_key?, password })
}

/// Finds the first valid `https://signal.group/#...` URL in a message's text.
pub fn find_group_invite_link(text: &str) -> Option<(&str, GroupInviteLink)> {
    let start = text.find("https://signal.group/#").or_else(|| text.find("http://signal.group/#"))?;
    let rest = &text[start..];
    let end = rest.find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'')).unwrap_or(rest.len());
    let candidate = rest[..end].trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | '!' | '?'));
    parse_group_invite_link(candidate).map(|parsed| (candidate, parsed))
}

/// Group attribute and permission edits sent to Signal Groups v2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupEdit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub member_access: Option<GroupAccess>,
    pub attributes_access: Option<GroupAccess>,
    pub announcements_only: Option<bool>,
}

/// Full membership lists for a Signal group, read from the decrypted group state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupMembers {
    pub members: Vec<GroupMember>,
    pub pending: Vec<GroupMember>,
    pub requesting: Vec<GroupMember>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupInfo {
    /// Public id, hex.
    pub id: String,
    pub title: String,
    pub description: String,
    pub members: u32,
    /// Whether this account is currently a member of the group.
    pub member: bool,
    /// Account ids of the members, including us. Empty when the wire has not said who.
    pub member_ids: Vec<String>,
    /// Account ids of the group's administrators.
    pub admins: Vec<String>,
    /// Who may add members to the group.
    pub member_access: GroupAccess,
    /// Who may edit the group's title, avatar, description, and disappearing timer.
    pub attributes_access: GroupAccess,
    /// Who may join from the group's invite link (`Any` = open, `Administrator` = approval required, `Unsatisfiable` = off).
    pub invite_link_access: GroupAccess,
    /// Only administrators may send messages (announcement group).
    pub announcements_only: bool,
    /// The group's `https://signal.group/#...` invite link when enabled. Empty when disabled or unknown.
    pub invite_link: String,
    /// Disappearing-message timer in seconds. 0 means off.
    pub timer_secs: u32,
}

/// Disappearing-message timer carried on a conversation or outgoing message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timer {
    pub seconds: u32,
    pub version: u32,
}

/// Per-conversation sync state from Signal's storage service (contact or group record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatState {
    pub peer: Peer,
    /// Milliseconds since 1970 until which the chat is muted. 0 = not muted; >= i64::MAX as u64 = forever.
    pub muted_until: u64,
    pub dont_notify_for_mentions_if_muted: bool,
    pub archived: bool,
    pub marked_unread: bool,
    pub blocked: bool,
    pub whitelisted: bool,
}

/// Verification state of a contact's Signal identity key (from storage service or `SyncMessage.verified`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    #[default]
    Default,
    Verified,
    Unverified,
}

/// A contact's computed 60-digit Signal safety number and verification status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyNumber {
    /// Raw 60 numeric digits (Signal fingerprint v2, 5200 iterations).
    pub digits: String,
    pub state: IdentityState,
    /// True when the contact was verified and their identity key changed without approval yet.
    pub changed: bool,
}

/// Standard iteration count for Signal numeric safety numbers (v1/v2).
pub const SAFETY_NUMBER_ITERATIONS: u32 = 5200;

/// Parses a lowercase dashed UUID string (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`) into 16 raw bytes.
pub fn aci_uuid_bytes(aci: &str) -> Option<[u8; 16]> {
    if !is_aci(aci) {
        return None;
    }
    let hex_only: String = aci.chars().filter(|&c| c != '-').collect();
    if hex_only.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex_only[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Computes Signal's 60-digit numeric safety number (fingerprint algorithm matching libsignal's
/// `DisplayableFingerprint`, 5200 SHA-512 iterations).
///
/// For Signal v2 ACI-based safety numbers, pass each side's 16-byte raw ACI UUID as `local_id` /
/// `remote_id` and their 33-byte serialized `IdentityKey` as `local_key` / `remote_key`.
pub fn compute_safety_number(local_id: &[u8], local_key: &[u8], remote_id: &[u8], remote_key: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    use std::fmt::Write as _;

    fn hash_half(stable_id: &[u8], key_bytes: &[u8]) -> String {
        let mut sha = Sha512::new();
        sha.update([0u8, 0u8]);
        sha.update(key_bytes);
        sha.update(stable_id);
        sha.update(key_bytes);
        let mut buf = sha.finalize();
        for _ in 1..SAFETY_NUMBER_ITERATIONS {
            let mut sha = Sha512::new();
            sha.update(&buf[..]);
            sha.update(key_bytes);
            buf = sha.finalize();
        }
        let mut out = String::with_capacity(30);
        for chunk in buf[..30].chunks_exact(5) {
            let val = chunk.iter().fold(0u64, |acc, &b| (acc << 8) | (b as u64));
            let _ = write!(out, "{:05}", val % 100_000);
        }
        out
    }

    let local = hash_half(local_id, local_key);
    let remote = hash_half(remote_id, remote_key);
    if local < remote {
        format!("{local}{remote}")
    } else {
        format!("{remote}{local}")
    }
}

/// Computes Signal's v2 ACI-based 60-digit safety number from two ACI strings and their serialized
/// 33-byte identity keys.
pub fn safety_number_for_acis(local_aci: &str, local_key: &[u8], remote_aci: &str, remote_key: &[u8]) -> Option<String> {
    if local_key.is_empty() || remote_key.is_empty() {
        return None;
    }
    let local_id = aci_uuid_bytes(local_aci)?;
    let remote_id = aci_uuid_bytes(remote_aci)?;
    Some(compute_safety_number(&local_id, local_key, &remote_id, remote_key))
}

/// Formats 60 safety-number digits into Signal's standard 12 groups of 5 digits.
pub fn format_safety_number(digits: &str) -> String {
    let clean: String = digits.chars().filter(|c| c.is_ascii_digit()).collect();
    if clean.is_empty() {
        return String::new();
    }
    clean
        .as_bytes()
        .chunks(5)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One contact record decrypted from Signal's Storage Service (`ContactRecord`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageContact {
    pub aci: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub phone: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub identity_state: IdentityState,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub whitelisted: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub marked_unread: bool,
    #[serde(default)]
    pub muted_until: u64,
    #[serde(default)]
    pub hidden: bool,
}

/// One Groups v2 record decrypted from Signal's Storage Service (`GroupV2Record`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageGroup {
    /// Group public id, hex (without `g.` prefix).
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub whitelisted: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub marked_unread: bool,
    #[serde(default)]
    pub muted_until: u64,
    #[serde(default)]
    pub dont_notify_for_mentions_if_muted: bool,
}

/// Account-level settings decrypted from Signal's Storage Service (`AccountRecord`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageAccount {
    /// Ordered conversation ids (`<aci>` or `g.<hex>`) pinned on the account.
    #[serde(default)]
    pub pinned: Vec<String>,
    #[serde(default)]
    pub keep_muted_archived: Option<bool>,
    #[serde(default)]
    pub link_previews: Option<bool>,
    /// Universal default disappearing-messages timer in seconds (0 = off).
    #[serde(default)]
    pub universal_expire_timer: u32,
    #[serde(default)]
    pub note_to_self_marked_unread: bool,
}

/// Full read-only snapshot decrypted from Signal's Storage Service manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageSnapshot {
    #[serde(default)]
    pub contacts: Vec<StorageContact>,
    #[serde(default)]
    pub groups: Vec<StorageGroup>,
    #[serde(default)]
    pub account: Option<StorageAccount>,
}

/// One Signal chat folder from the storage service (`ChatFolderRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatFolder {
    pub id: String,
    pub name: String,
    pub position: u32,
    pub show_only_unread: bool,
    pub show_muted: bool,
    pub include_all_individual: bool,
    pub include_all_groups: bool,
    pub included: Vec<Peer>,
    pub excluded: Vec<Peer>,
}

/// Response to a Signal message request (`SyncMessage.MessageRequestResponse.Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRequestAction {
    Accept,
    Delete,
    Block,
    BlockAndDelete,
    Spam,
    BlockAndSpam,
}

/// The earlier message a message answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quote {
    pub sent: u64,
    pub author: String,
    pub text: String,
    /// Styles of `text` after any mention in the quote was substituted.
    pub spans: Vec<Span>,
}

/// One link preview carried on a Signal `DataMessage.preview`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkPreview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub date: u64,
    /// Decrypted preview thumbnail bytes (JPEG/PNG/WebP), when attached.
    pub image: Option<Vec<u8>>,
    pub image_width: u32,
    pub image_height: u32,
}

/// Returns true only when `url` is a valid `http://` or `https://` URL that actually appears
/// as a complete URL token in the message `body` (matching Signal Desktop/Android/iOS validation
/// and preventing prefix-domain spoofing such as `https://example.co` matching `https://example.com`).
pub fn preview_url_in_body(body: &str, url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    let mut offset = 0;
    while let Some(rel) = body[offset..].find(url) {
        let start = offset + rel;
        let end = start + url.len();
        let before_ok = body[..start].chars().next_back().is_none_or(|ch| {
            ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | ',' | ';')
        });
        let tail = &body[end..];
        let after_ok = match tail.chars().next() {
            None => true,
            Some(ch) if ch.is_whitespace() || matches!(ch, ')' | ']' | '}' | '>' | '"' | '\'' | ',' | ';' | '!') => true,
            Some('.') => tail[1..].chars().next().is_none_or(|next| {
                next.is_whitespace() || matches!(next, ')' | ']' | '}' | '>' | '"' | '\'')
            }),
            Some(_) => false,
        };
        if before_ok && after_ok {
            return true;
        }
        offset = start + 1;
        while offset < body.len() && !body.is_char_boundary(offset) {
            offset += 1;
        }
    }
    false
}

/// One message's words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub author: String,
    /// The author's name as the user knows them: their contact name, else their profile's.
    pub name: String,
    /// Milliseconds since 1970: Signal's own id for the message.
    pub sent: u64,
    /// Mentions already read as `@Name`.
    pub text: String,
    pub quote: Option<Quote>,
    pub mentions_me: bool,
    /// Styles of `text`, byte offsets, after mention substitution.
    pub spans: Vec<Span>,
    /// What it carries that is not text and is not drawn here ("Photo", "Sticker"), in words.
    pub unsupported: Option<String>,
    /// Disappearing-message timer in seconds carried on this message. 0 means off.
    pub expire_timer: u32,
    /// Timer version carried on this message. 0 when unset.
    pub expire_timer_version: u32,
    /// Shared contact cards attached to this message.
    pub contacts: Vec<protocol::SharedContact>,
    /// Link previews attached to this message (`DataMessage.preview`).
    pub previews: Vec<LinkPreview>,
}

/// Longest payment note shown in its label: what a sender typed joins the words after an em dash.
/// The reply snippet in `signal.rs` keeps the same room.
const NOTE_LIMIT: usize = 200;

/// What a payment says (#733): where it is acted on, and the note the sender wrote, when one came.
/// The amount itself rides inside an encrypted receipt only a wallet can read, so nothing of it is
/// shown. The real wire and the fake one say the same words, so a scripted test exercises what a
/// person is told.
pub fn payment_label(note: Option<&str>) -> String {
    let mut label = "Payment · open Signal on your phone".to_owned();
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        label.push_str(" — ");
        label.push_str(&note.chars().take(NOTE_LIMIT).collect::<String>());
    }
    label
}

/// What a gift badge says (#733): redeeming it presents a receipt credential to Signal's server,
/// and that is the phone's work.
pub fn gift_badge_label() -> &'static str {
    "Gift badge · open Signal on your phone to redeem"
}

/// What a view-once photo or video says while its media cannot be opened from here (#748): what
/// it is, and where it opens. The flag goes with an attachment, and the attachment is not a
/// "Photo" that opens to nothing. The real wire and the fake one say the same words, so a
/// scripted test exercises what a person is told.
pub fn view_once_label(content_type: Option<&str>) -> &'static str {
    match content_type.and_then(|kind| kind.split('/').next()) {
        Some("image") => "View-once photo · open it on your phone",
        Some("video") => "View-once video · open it on your phone",
        _ => "View-once media · open it on your phone",
    }
}

/// The note under a long text's body until attachments can be fetched: what is here is its
/// first part, the rest on the phone.
pub const CONTINUES: &str = "\n\n(message continues on your phone)";

/// Something Signal sent unasked, or the end of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The contacts the phone shared, whole.
    Contacts(Vec<Contact>),
    /// The groups the account is in, whole. This is a snapshot: it does not say what changed.
    Groups(Vec<GroupInfo>),
    /// One group changed after that snapshot. `changes` is one system line each.
    GroupChanged {
        peer: Peer,
        /// Who did it, when the change names them. Empty when it does not.
        by: String,
        changes: Vec<String>,
        /// Set when the name changed, so the row can take it without reading the line.
        title: Option<String>,
        /// Set when the description changed.
        description: Option<String>,
        /// This account is no longer a member.
        left: bool,
        /// This account was added back to the group.
        rejoined: bool,
    },
    /// A message from someone else, or one we sent from another device (`outgoing`).
    Message { peer: Peer, text: Text, outgoing: bool },
    /// A message was edited. `target` is when it was first sent, `at` when the edit was made.
    Edited { peer: Peer, author: String, target: u64, at: u64, text: String, spans: Vec<Span> },
    Deleted { peer: Peer, author: String, target: u64 },
    /// Removed on another device for this account only. `target` is when it was first sent.
    DeletedForMe { peer: Peer, author: String, target: u64 },
    /// The whole conversation was removed for this account on another device.
    Forgot(Peer),
    /// `by` reacted to `author`'s message sent at `target`.
    Reacted { peer: Peer, by: String, author: String, target: u64, emoji: String, removed: bool },
    /// A poll was started. `options` are the choices, in order.
    Poll {
        peer: Peer,
        author: String,
        name: String,
        sent: u64,
        question: String,
        options: Vec<String>,
        multiple: bool,
        outgoing: bool,
    },
    /// `by` voted on `author`'s poll sent at `target`. An empty `indexes` takes the vote back.
    /// `serial` is Signal's voteCount; `0` means this vote is newer than the one we have.
    PollVote { peer: Peer, by: String, author: String, target: u64, indexes: Vec<u32>, serial: u32 },
    /// `author` ended their poll sent at `target`.
    PollEnded { peer: Peer, author: String, target: u64 },
    /// The user read these messages (author, sent) on another device.
    ReadElsewhere(Vec<(String, u64)>),
    /// `by`'s phone received our messages sent at these times, or `by` read them (`read`).
    /// `viewed` is a voice note or a video played: it counts as read, and says when.
    /// `at` is when this receipt was sent, in milliseconds.
    Receipt { by: String, sent: Vec<u64>, read: bool, viewed: bool, at: u64 },
    Typing { peer: Peer, by: String, name: String, started: bool },
    /// Someone is calling: an offer for the person (`by`) in a one-to-one conversation. A
    /// group's ring update is a blob this library cannot open, so a group call stays with the
    /// phone. `name` is the caller's as the user knows them, `sent` when the offer was sent.
    Call { peer: Peer, by: String, name: String, video: bool, id: u64, sent: u64 },
    /// A call we heard of is over: its hangup, or the other side telling us it was busy.
    /// `answered` says someone took it on another device; nobody did when it is false.
    CallEnded { id: u64, answered: bool },
    /// A call synced from the user's phone (`SyncMessage.callEvent`).
    CallEvent {
        peer: Peer,
        call_id: u64,
        timestamp: u64,
        kind: CallKind,
        incoming: bool,
        outcome: CallOutcome,
    },
    /// A call log action synced from the user's phone (`SyncMessage.callLogEvent`).
    CallLogEvent {
        kind: CallLogKind,
        peer: Option<Peer>,
        call_id: Option<u64>,
        timestamp: u64,
    },
    /// A call link created or updated on another device (`SyncMessage.callLinkUpdate`).
    CallLinkUpdated {
        root_key: [u8; 32],
        admin_passkey: Option<Vec<u8>>,
    },
    /// The user's own settings, as the phone shares them.
    Settings { read_receipts: bool, typing: bool, link_previews: Option<bool> },
    /// A conversation's disappearing-messages timer changed (0 turns it off).
    TimerChanged { peer: Peer, seconds: u32, version: u32 },
    /// Per-conversation mute, archive, pin, and mark-unread state from the phone / storage service.
    ChatStates {
        chats: Vec<ChatState>,
        /// Ordered list of pinned conversations, when present in the account record.
        pinned: Option<Vec<Peer>>,
        /// Account setting: keep muted chats archived when new messages arrive.
        keep_muted_archived: Option<bool>,
    },
    /// Read-only Storage Service snapshot (contacts, groups, account record) fetched on connect or `fetchLatest`.
    StorageSync(StorageSnapshot),
    /// Chat folders synced from the phone's storage service.
    Folders(Vec<ChatFolder>),
    /// Block list synced from the phone (`SyncMessage.blocked`): blocked ACIs and group public ids.
    Blocked { people: Vec<String>, groups: Vec<String> },
    /// A message request was answered on another device (`SyncMessage.messageRequestResponse`).
    MessageRequestResponse { peer: Peer, action: MessageRequestAction },
    /// A contact's identity key changed (`IdentityChange::ReplacedExisting`).
    IdentityChanged { peer: Peer },
    /// A contact's verification state changed (locally or synced via `SyncMessage.verified`).
    VerifiedChanged { aci: String, state: IdentityState },
    /// `by` pinned `author`'s message sent at `target`. `duration_secs == None` means forever.
    Pinned { peer: Peer, by: String, author: String, target: u64, duration_secs: Option<u32> },
    /// `by` unpinned `author`'s message sent at `target`.
    Unpinned { peer: Peer, by: String, author: String, target: u64 },
    /// Everything that waited while we were away has arrived.
    Caught,
    /// The connection is gone; a new wire is needed.
    Closed(String),
    /// The phone removed this device, or the account is gone. Signing in again links anew.
    Ended(String),
}

/// Kind of call in a synced `CallEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Voice,
    Video,
    Group,
    AdHoc,
}

/// Outcome of a synced `CallEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Accepted,
    NotAccepted,
    Deleted,
    Observed,
}

/// Action in a synced `CallLogEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallLogKind {
    Clear,
    MarkedAsRead,
    MarkedAsReadInConversation,
    ClearInConversation,
}

/// A Signal call link (`https://signal.link/call/#key=...`) and its settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallLink {
    pub root_key: [u8; 32],
    pub url: String,
    pub name: String,
    pub approve_all: bool,
    pub admin_passkey: Option<Vec<u8>>,
}

/// Result of joining a Signal call link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallLinkJoin {
    pub room_id: String,
    pub name: String,
    pub waiting_for_approval: bool,
}

/// Formats a 32-byte call link root key into a `https://signal.link/call/#key=...` URL.
pub fn format_call_link(root_key: &[u8; 32]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(root_key);
    format!("https://signal.link/call/#key={encoded}")
}

/// Parses a `https://signal.link/call/#key=...` URL into its 32-byte root key.
pub fn parse_call_link(url: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    let trimmed = url.trim();
    let frag = trimmed
        .strip_prefix("https://signal.link/call/#key=")
        .or_else(|| trimmed.strip_prefix("http://signal.link/call/#key="))?;
    if frag.is_empty() {
        return None;
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(frag)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(frag))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(frag))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(frag))
        .ok()?;
    raw.try_into().ok()
}

/// Finds the first valid `https://signal.link/call/#key=...` URL in a message's text.
pub fn find_call_link(text: &str) -> Option<(&str, [u8; 32])> {
    let start = text.find("https://signal.link/call/#key=").or_else(|| text.find("http://signal.link/call/#key="))?;
    let rest = &text[start..];
    let end = rest.find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'')).unwrap_or(rest.len());
    let candidate = rest[..end].trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | '!' | '?'));
    parse_call_link(candidate).map(|key| (candidate, key))
}

/// Derives a deterministic SFU room identifier from a call link's 32-byte root key using HKDF-SHA256.
pub fn call_link_room_id(root_key: &[u8; 32]) -> String {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, root_key);
    let mut okm = [0u8; 32];
    let _ = hk.expand(b"Signal_CallLink_RoomId_v1", &mut okm);
    okm.iter().map(|b| format!("{b:02x}")).collect()
}

/// One GIF returned by Signal's Giphy proxy search/trending endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GifEntry {
    pub id: String,
    pub title: String,
    pub preview_url: String,
    pub mp4_url: String,
    pub width: u32,
    pub height: u32,
}

/// One file on a message we send. The bytes are what Signal is given: a still picture has already
/// had its metadata removed, unless the person asked for the file as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// A small JPEG a conversation can show while the file arrives. Empty when there is none.
    pub thumb: Vec<u8>,
    pub blurhash: Option<String>,
    /// True for animated GIFs (including looping MP4 GIFs from the GIF picker).
    pub gif: bool,
}

/// What is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Text { text: String, quote: Option<Quote>, ranges: Vec<Range>, timer: Timer, previews: Vec<LinkPreview> },
    /// Files, with the caption in `text` (empty when there is none) and optional body `ranges`.
    Files { text: String, quote: Option<Quote>, ranges: Vec<Range>, files: Vec<File>, timer: Timer },
    React { author: String, target: u64, emoji: String, remove: bool },
    Edit { target: u64, text: String, ranges: Vec<Range>, timer: Timer },
    /// Delete for everyone.
    Delete { target: u64 },
    /// Delete for me: the user's other devices drop this message, and nobody else is told.
    DeleteForMe { author: String, target: u64 },
    /// Start a poll. `options` has 2 to 10 choices.
    Poll { question: String, options: Vec<String>, multiple: bool, timer: Timer },
    /// Vote, or take a vote back when `indexes` is empty. `serial` increments each time we vote.
    PollVote { author: String, target: u64, indexes: Vec<u32>, serial: u32 },
    /// End our own poll.
    PollEnd { target: u64 },
    /// Share a contact card.
    Contact { contact: protocol::SharedContact, timer: Timer },
    /// Pin a message in a conversation (`duration_secs == None` pins forever).
    Pin { author: String, target: u64, duration_secs: Option<u32> },
    /// Unpin a message in a conversation.
    Unpin { author: String, target: u64 },
}

/// Why something failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The session is over: this device was unlinked. Only the real wire can tell.
    #[cfg_attr(not(feature = "signal"), allow(dead_code))]
    Ended(String),
    /// Signal said no, in words the user can read.
    Refused(String),
    /// No answer.
    Net(String),
}

impl Fault {
    pub fn words(&self) -> String {
        match self {
            Fault::Ended(_) => "This computer is no longer linked to Signal. Sign in again.".to_owned(),
            Fault::Refused(why) => why.clone(),
            Fault::Net(_) => "Signal did not answer.".to_owned(),
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Ended(why) | Fault::Refused(why) | Fault::Net(why) => f.write_str(why),
        }
    }
}

/// One connection to Signal under one linked device.
pub trait Wire: Send + Sync {
    /// The events, once: whoever takes them is the only reader.
    fn events(&self) -> Option<Receiver<Event>>;
    /// Our account id and our own name.
    fn me(&self) -> (String, String);
    /// Sends, and answers the time it went with: the message's id from now on.
    fn send(&self, to: &Peer, body: Body) -> Result<u64, Fault>;
    /// People the last group send did not reach. Empty when the wire cannot say who: a hard
    /// error fails the whole send instead.
    fn missed(&self) -> Vec<String> {
        Vec::new()
    }
    fn typing(&self, to: &Peer, started: bool) -> Result<(), Fault>;
    /// The user read these (author, sent): the user's other devices are told, and with `receipts`
    /// the authors too.
    fn read(&self, read: &[(String, u64)], receipts: bool) -> Result<(), Fault>;
    /// Removes this device from the account, and what it kept here with it.
    fn unlink(&self) -> Result<(), Fault>;
    /// A person's profile picture or a group's, opened: `None` when they have none, or we hold
    /// no key to their profile.
    fn picture(&self, of: &Peer) -> Result<Option<Vec<u8>>, Fault>;
    /// Our own card: name, about, about emoji, username, phone, and a signal.me link when one
    /// is known. A username the library cannot read is empty, and so is the link.
    fn card(&self) -> Result<OwnCard, Fault>;
    /// Writes the name, about, and about emoji. `None` leaves that part. An empty about or
    /// emoji clears it. Succeeds only once the written profile was read back and the other
    /// devices were told.
    fn set_card(&self, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Fault>;
    /// The account id behind a username (`name.42`), or none when Signal has no such username.
    /// A phone number is not looked up here.
    fn lookup(&self, username: &str) -> Result<Option<String>, Fault>;
    /// Sets a conversation's mute expiry in milliseconds since 1970 (0 unmutes; i64::MAX mutes forever).
    fn set_mute(&self, to: &Peer, until_ms: u64) -> Result<(), Fault>;
    /// Archives or unarchives a conversation.
    fn set_archived(&self, to: &Peer, archived: bool) -> Result<(), Fault>;
    /// Pins or unpins a conversation.
    fn set_pinned(&self, to: &Peer, pinned: bool) -> Result<(), Fault>;
    /// Marks a conversation as unread (or clears the manual unread flag).
    fn set_marked_unread(&self, to: &Peer, unread: bool) -> Result<(), Fault>;
    /// Blocks or unblocks a person or group, syncing the updated block list to other devices.
    fn set_blocked(&self, peer: &Peer, blocked: bool) -> Result<(), Fault>;
    /// Responds to a message request (Accept, Delete, Block, Spam, etc.) and syncs to other devices.
    fn respond_message_request(&self, peer: &Peer, action: MessageRequestAction) -> Result<(), Fault>;
    /// Creates a new Signal Groups v2 group with `title` and `members` (ACIs) and returns its info.
    fn create_group(&self, title: &str, members: &[String]) -> Result<GroupInfo, Fault>;
    /// Leaves a Signal Groups v2 group by its public hex id.
    fn leave_group(&self, group_id: &str) -> Result<(), Fault>;
    /// Lists a group's members (with admin flags), pending invites, and join requests.
    fn group_members(&self, _group_id: &str) -> Result<GroupMembers, Fault> {
        Err(Fault::Refused("Group members are not available.".to_owned()))
    }
    /// Adds or invites people (ACIs) to a group.
    fn group_add_members(&self, _group_id: &str, _people: &[String]) -> Result<(), Fault> {
        Err(Fault::Refused("Adding group members is not available.".to_owned()))
    }
    /// Removes a member (ACI) from a group.
    fn group_remove_member(&self, _group_id: &str, _person: &str) -> Result<(), Fault> {
        Err(Fault::Refused("Removing group members is not available.".to_owned()))
    }
    /// Promotes a member to admin or revokes their admin role.
    fn group_set_admin(&self, _group_id: &str, _person: &str, _admin: bool) -> Result<(), Fault> {
        Err(Fault::Refused("Changing group admin roles is not available.".to_owned()))
    }
    /// Updates a group's title, description, or permissions.
    fn group_update(&self, _group_id: &str, _edit: GroupEdit) -> Result<(), Fault> {
        Err(Fault::Refused("Editing group settings is not available.".to_owned()))
    }
    /// Sets (or clears when `bytes` is empty) a group's avatar.
    fn group_set_avatar(&self, _group_id: &str, _bytes: Vec<u8>) -> Result<(), Fault> {
        Err(Fault::Refused("Changing the group photo is not available.".to_owned()))
    }
    /// Sets a group's disappearing-messages timer (0 turns it off).
    fn group_set_timer(&self, _group_id: &str, _seconds: u32) -> Result<(), Fault> {
        Err(Fault::Refused("Changing the group timer is not available.".to_owned()))
    }
    /// Enables, disables, requires approval for, or resets a group's invite link.
    /// Returns the active `https://signal.group/#...` link (or empty string when disabled).
    fn group_invite_link(&self, _group_id: &str, _mode: InviteLinkMode) -> Result<String, Fault> {
        Err(Fault::Refused("Group invite links are not available.".to_owned()))
    }
    /// Joins (or requests to join) a group from its `https://signal.group/#...` invite link.
    fn group_join_by_link(&self, _url: &str) -> Result<GroupInfo, Fault> {
        Err(Fault::Refused("Joining a group by link is not available.".to_owned()))
    }
    /// Approves or denies a member's request to join a group.
    fn group_review_request(&self, _group_id: &str, _person: &str, _approve: bool) -> Result<(), Fault> {
        Err(Fault::Refused("Reviewing join requests is not available.".to_owned()))
    }
    /// Accepts or declines a pending invite to a group for our own account.
    fn group_pending_invite(&self, _group_id: &str, _accept: bool) -> Result<(), Fault> {
        Err(Fault::Refused("Answering group invites is not available.".to_owned()))
    }
    /// Searches Giphy (or trending when `query` is empty) through Signal's content proxy.
    fn search_gifs(&self, _query: &str, _offset: u32) -> Result<(Vec<GifEntry>, Option<u32>), Fault> {
        Err(Fault::Refused("GIF search is not available.".to_owned()))
    }
    /// Downloads a GIF MP4 rendition through Signal's content proxy.
    fn fetch_gif(&self, _url: &str) -> Result<Vec<u8>, Fault> {
        Err(Fault::Refused("Downloading GIFs is not available.".to_owned()))
    }
    /// Creates a new Signal call link with an optional name and admin-approval switch.
    fn create_call_link(&self, _name: &str, _approve_all: bool) -> Result<CallLink, Fault> {
        Err(Fault::Refused("Call links are not available.".to_owned()))
    }
    /// Updates a Signal call link's name or admin-approval switch.
    fn update_call_link(&self, _root_key: &[u8; 32], _name: Option<String>, _approve_all: Option<bool>) -> Result<CallLink, Fault> {
        Err(Fault::Refused("Updating call links is not available.".to_owned()))
    }
    /// Deletes a Signal call link.
    fn delete_call_link(&self, _root_key: &[u8; 32]) -> Result<(), Fault> {
        Err(Fault::Refused("Deleting call links is not available.".to_owned()))
    }
    /// Joins a Signal call link and returns its derived SFU room id and approval state.
    fn join_call_link(&self, _url: &str) -> Result<CallLinkJoin, Fault> {
        Err(Fault::Refused("Joining call links is not available.".to_owned()))
    }
    /// Computes the 60-digit Signal safety number and verification state for a contact ACI.
    fn safety_number(&self, _aci: &str) -> Result<Option<SafetyNumber>, Fault> {
        Ok(None)
    }
    /// Sets a contact's verification state (Default, Verified, Unverified), clearing any
    /// unapproved key-change block and syncing `SyncMessage.verified` to other devices.
    fn set_verified(&self, _aci: &str, _state: IdentityState) -> Result<(), Fault> {
        Err(Fault::Refused("Safety number verification is not available.".to_owned()))
    }
    /// Fetches and decrypts another person's versioned Signal profile using their stored profile key.
    fn person_profile(&self, _aci: &str) -> Result<PersonProfile, Fault> {
        Ok(PersonProfile::default())
    }
    /// Fetches a link preview strictly through Signal's content proxy (NEVER directly).
    fn fetch_link_preview(&self, _url: &str) -> Result<Option<LinkPreview>, Fault> {
        Ok(None)
    }
    /// Ends the connection: the events end, and every later request fails.
    fn close(&self);
}

/// Our own profile, as the wire can read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnCard {
    pub name: String,
    pub about: String,
    pub emoji: String,
    /// Empty when this build cannot read one.
    pub username: String,
    /// International form. Empty when the registration did not keep one.
    pub phone: String,
    /// A signal.me link when a username is known. Empty otherwise. The shell draws the QR.
    pub link: String,
}

/// Another person's decrypted versioned Signal profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonProfile {
    pub name: String,
    pub about: String,
    pub emoji: String,
    /// Visible donation / sustainer badges reported by Signal.
    pub badges: Vec<String>,
    /// False when no profile key is stored for this person yet ("Name not shared yet").
    pub key_known: bool,
}

/// What a finished link produced.
pub struct Linked {
    /// What the keychain keeps (`super::Saved`, as JSON).
    pub session: String,
}

/// Links this computer to an account as a new device: shows the link to scan, waits for the
/// phone, and answers what to keep. Blocks for as long as that takes. A link the user cancels
/// (`cancelled`) is undone, never left on the account.
pub trait Linker: Send {
    fn link(self: Box<Self>, device: &str, show: &dyn Fn(String), cancelled: Arc<AtomicBool>) -> Result<Linked, Fault>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_go_both_ways_and_nothing_else_reads_as_one() {
        let aci = "0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b";
        assert_eq!(Peer::parse(aci), Some(Peer::Direct(aci.to_owned())));
        assert_eq!(Peer::parse("g.00ff"), Some(Peer::Group("00ff".into())));
        assert_eq!(Peer::Group("00ff".into()).conversation(), "g.00ff");
        assert_eq!(Peer::parse("g."), None);
        assert_eq!(Peer::parse("0F6C2A3E-1B4D-4E5F-8A9B-0C1D2E3F4A5B"), None);
        assert_eq!(parse_message_id(&message_id(1_791_892_800_123, aci)), Some((1_791_892_800_123, aci.to_owned())));
        assert_eq!(parse_message_id("17-not-an-aci"), None);
    }

    #[test]
    fn a_payment_and_a_gift_badge_say_where_they_are_acting() {
        assert_eq!(payment_label(None), "Payment · open Signal on your phone");
        assert_eq!(payment_label(Some("for coffee")), "Payment · open Signal on your phone — for coffee");
        // An empty or blank note is no note.
        assert_eq!(payment_label(Some("")), "Payment · open Signal on your phone");
        assert_eq!(payment_label(Some("   ")), "Payment · open Signal on your phone");
        // A note is a peer's words: capped, never unbounded.
        let long = "x".repeat(NOTE_LIMIT + 50);
        let said = payment_label(Some(long.as_str()));
        assert_eq!(said.chars().count(), "Payment · open Signal on your phone — ".chars().count() + NOTE_LIMIT);
        assert_eq!(gift_badge_label(), "Gift badge · open Signal on your phone to redeem");
    }

    #[test]
    fn a_view_once_message_says_where_it_opens() {
        assert_eq!(view_once_label(Some("image/jpeg")), "View-once photo · open it on your phone");
        assert_eq!(view_once_label(Some("video/mp4")), "View-once video · open it on your phone");
        assert_eq!(view_once_label(Some("application/pdf")), "View-once media · open it on your phone");
        assert_eq!(view_once_label(None), "View-once media · open it on your phone");
    }

    #[test]
    fn group_invite_link_round_trips_and_extracts_from_text() {
        let key = [0x5au8; 32];
        let pass = b"secret-pass-1234";
        let url = format_group_invite_link(&key, pass);
        assert!(url.starts_with("https://signal.group/#"));
        let parsed = parse_group_invite_link(&url).expect("valid invite link");
        assert_eq!(parsed.master_key, key);
        assert_eq!(parsed.password, pass);

        let msg = format!("Come join us: {url}!");
        let (found_url, found) = find_group_invite_link(&msg).expect("found in text");
        assert_eq!(found_url, url);
        assert_eq!(found, parsed);

        assert!(parse_group_invite_link("https://signal.group/#not-valid-base64!!!").is_none());
        assert!(parse_group_invite_link("https://example.com/#abc").is_none());
    }

    #[test]
    fn call_link_round_trips_derives_room_id_and_extracts_from_text() {
        let key = [0x42u8; 32];
        let url = format_call_link(&key);
        assert!(url.starts_with("https://signal.link/call/#key="));
        assert_eq!(parse_call_link(&url), Some(key));
        let room1 = call_link_room_id(&key);
        let room2 = call_link_room_id(&[0x43u8; 32]);
        assert_eq!(room1.len(), 64);
        assert_ne!(room1, room2);
        let msg = format!("Hop on: {url}");
        let (found_url, found_key) = find_call_link(&msg).expect("found in text");
        assert_eq!(found_url, url);
        assert_eq!(found_key, key);
        assert!(parse_call_link("https://signal.link/call/#key=too-short").is_none());
    }

    #[test]
    fn safety_number_matches_libsignal_test_vector_and_formats_in_groups_of_five() {
        let decode_hex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        // Official libsignal test vector (`rust/protocol/src/fingerprint.rs` testVectorsVersion1).
        let alice_key = decode_hex("0506863bc66d02b40d27b8d49ca7c09e9239236f9d7d25d6fcca5ce13c7064d868");
        let bob_key = decode_hex("05f781b6fb32fed9ba1cf2de978d4d5da28dc34046ae814402b5c0dbd96fda907b");
        let expected = "300354477692869396892869876765458257569162576843440918079131";

        let ab = compute_safety_number(b"+14152222222", &alice_key, b"+14153333333", &bob_key);
        let ba = compute_safety_number(b"+14153333333", &bob_key, b"+14152222222", &alice_key);
        assert_eq!(ab, expected);
        assert_eq!(ba, expected);
        assert_eq!(ab.len(), 60);

        let formatted = format_safety_number(&ab);
        let groups: Vec<&str> = formatted.split(' ').collect();
        assert_eq!(groups.len(), 12);
        assert!(groups.iter().all(|g| g.len() == 5 && g.bytes().all(|b| b.is_ascii_digit())));

        // ACI v2 symmetry with 16-byte UUID identifiers.
        let a_aci = "a1a1a1a1-0000-4000-8000-000000000001";
        let b_aci = "b2b2b2b2-0000-4000-8000-000000000002";
        let v2_ab = safety_number_for_acis(a_aci, &alice_key, b_aci, &bob_key).unwrap();
        let v2_ba = safety_number_for_acis(b_aci, &bob_key, a_aci, &alice_key).unwrap();
        assert_eq!(v2_ab.len(), 60);
        assert_eq!(v2_ab, v2_ba);
    }

    #[test]
    fn preview_url_must_appear_in_message_body() {
        assert!(preview_url_in_body("See https://example.org/post for details", "https://example.org/post"));
        assert!(!preview_url_in_body("See https://example.org/post for details", "https://evil.example.net/phish"));
        assert!(!preview_url_in_body("No links here", "https://example.org/post"));
        assert!(!preview_url_in_body("javascript:alert(1)", "javascript:alert(1)"));
        assert!(!preview_url_in_body("anything", ""));
    }
}
