//! The seam between the adapter and the Signal library. Everything above it speaks the contract;
//! everything below it is Signal: keys, sessions, sealed sender, the socket. `real` is presage
//! (built with the `signal` feature); `fake` is a script, so the adapter can be run through
//! chatd's socket with no Signal behind it and no Signal library linked.
//!
//! Signal names a message by who wrote it and the millisecond they sent it, and has no list of
//! conversations or on-demand history on its servers: what arrives live — plus any encrypted
//! message history archive transferred from the primary phone at link time — is all there is.

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
    /// True when the user turned Signal Stories off in their account settings.
    #[serde(default)]
    pub stories_disabled: Option<bool>,
    /// True when story view receipts (`storyViewReceiptsEnabled`) are enabled on the account.
    #[serde(default)]
    pub story_view_receipts: Option<bool>,
}

/// One Signal story distribution list (`StoryDistributionListRecord` from Storage Service).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoryDistributionList {
    /// Hex-encoded 16-byte distribution list identifier (`00000000...` is "My Story").
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Recipient ACIs named in this list (excluded when `is_block_list == true`, included otherwise).
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default)]
    pub allows_replies: bool,
    /// When true (the default for "My Story" with exceptions), send to all contacts EXCEPT
    /// `recipients`. When false (an allowlist / custom list), send ONLY to `recipients`.
    #[serde(default)]
    pub is_block_list: bool,
}

/// Resolves the exact sorted, deduplicated set of recipient ACIs permitted to receive a story
/// posted to `list`, strictly enforcing allowlist vs blocklist semantics and never leaking to
/// blocked contacts or ourselves.
pub fn resolve_story_recipients(
    list: &StoryDistributionList,
    all_contacts: &[String],
    blocked: &std::collections::HashSet<String>,
    me: &str,
) -> Vec<String> {
    let mut out: Vec<String> = if list.is_block_list {
        let excluded: std::collections::HashSet<&str> = list.recipients.iter().map(String::as_str).collect();
        all_contacts
            .iter()
            .filter(|aci| is_aci(aci) && aci.as_str() != me && !blocked.contains(aci.as_str()) && !excluded.contains(aci.as_str()))
            .cloned()
            .collect()
    } else {
        list.recipients
            .iter()
            .filter(|aci| is_aci(aci) && aci.as_str() != me && !blocked.contains(aci.as_str()))
            .cloned()
            .collect()
    };
    out.sort();
    out.dedup();
    out
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
    #[serde(default)]
    pub distribution_lists: Vec<StoryDistributionList>,
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

/// Signal `AttachmentPointer.Flags` bitmask constants (`SignalService.proto`).
pub const FLAG_VOICE_MESSAGE: u32 = 1;
pub const FLAG_BORDERLESS: u32 = 2;
pub const FLAG_GIF: u32 = 8;

/// An encrypted attachment pointer carried on a Signal `DataMessage` or `Quote`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pointer {
    pub cdn_number: u32,
    pub cdn_id: Option<u64>,
    pub cdn_key: Option<String>,
    pub content_type: String,
    /// 64-byte attachment key (`aes_key[0..32] || hmac_key[32..64]`).
    pub key: Vec<u8>,
    /// Unpadded plaintext size in bytes (0 when omitted).
    pub size: u32,
    /// SHA-256 digest of the encrypted blob on Signal's CDN.
    pub digest: Vec<u8>,
    pub file_name: String,
    /// Bitmask of `FLAG_VOICE_MESSAGE`, `FLAG_BORDERLESS`, `FLAG_GIF`.
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub caption: String,
    pub blurhash: Option<String>,
    /// Inline small thumbnail bytes if the sender included them.
    pub thumbnail: Vec<u8>,
    pub upload_timestamp: u64,
    /// Direct URL override used by local CDN stubs in scripted tests (never set by real Signal).
    pub url: Option<String>,
}

impl Pointer {
    /// True when this pointer identifies a downloadable blob on Signal's CDN or a local stub.
    pub fn downloadable(&self) -> bool {
        self.cdn_id.is_some()
            || self.cdn_key.as_deref().is_some_and(|k| !k.is_empty())
            || self.url.as_deref().is_some_and(|u| !u.is_empty())
    }

    pub fn is_voice(&self) -> bool {
        self.flags & FLAG_VOICE_MESSAGE != 0
    }

    pub fn is_gif(&self) -> bool {
        self.flags & FLAG_GIF != 0 || self.content_type.eq_ignore_ascii_case("image/gif")
    }
}

/// An attachment preview inside a Signal `DataMessage.quote`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotedAttachment {
    pub content_type: String,
    pub file_name: String,
    pub thumbnail: Option<Pointer>,
}

/// The earlier message a message answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quote {
    pub sent: u64,
    pub author: String,
    pub text: String,
    /// Styles of `text` after any mention in the quote was substituted.
    pub spans: Vec<Span>,
    /// Quoted attachment metadata and optional thumbnail pointers (`Quote.attachments`).
    pub attachments: Vec<QuotedAttachment>,
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

/// A story text preview is kept only for an `https://` URL that appears in the story text.
/// `http://` is not enough: story fetches go through Signal's content proxy, which is https-only.
pub fn story_preview_kept(body: &str, url: &str) -> bool {
    let url = url.trim();
    url.starts_with("https://") && preview_url_in_body(body, url)
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
    /// Encrypted media/file attachments carried on this message (`DataMessage.attachments`).
    pub attachments: Vec<Pointer>,
    /// Optional Signal sticker (`DataMessage.sticker`).
    pub sticker: Option<Sticker>,
    /// Story this message replies to (`DataMessage.story_context`).
    pub story_context: Option<StoryContext>,
}

/// Reference to the Signal story that a reply or reaction targets (`DataMessage.story_context`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryContext {
    pub author: String,
    pub sent: u64,
}

/// Text content and styling of a Signal text story (`TextAttachment`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryText {
    pub text: String,
    pub style: String,
    pub fg_color: u32,
    pub bg_color: u32,
    pub gradient_colors: Vec<u32>,
    pub preview: Option<LinkPreview>,
}

/// One incoming or synced Signal Story (`ContentBody::StoryMessage`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryMessageData {
    pub author: String,
    pub name: String,
    pub sent: u64,
    /// Public hex group id when posted to a group story (`StoryMessage.group`).
    pub group_id: Option<String>,
    pub allows_replies: bool,
    pub file: Option<Pointer>,
    pub text: Option<StoryText>,
    pub outgoing: bool,
}

/// One sticker carried on a Signal `DataMessage.sticker`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sticker {
    /// 16-byte sticker pack id, lowercase hex (32 chars).
    pub pack_id: String,
    /// 32-byte sticker pack key, lowercase hex (64 chars).
    pub pack_key: String,
    pub sticker_id: u32,
    pub emoji: String,
    pub data: Pointer,
}

/// One sticker entry inside a Signal sticker pack manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StickerEntry {
    pub id: u32,
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub content_type: String,
}

/// A decrypted Signal sticker pack manifest and installation state.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StickerPackInfo {
    /// 16-byte pack id, lowercase hex (32 chars).
    pub id: String,
    /// 32-byte pack key, lowercase hex (64 chars).
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub cover_id: Option<u32>,
    #[serde(default)]
    pub stickers: Vec<StickerEntry>,
    #[serde(default)]
    pub installed: bool,
}

fn is_hex_of_len(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Formats a `https://signal.art/addstickers/#pack_id=...&pack_key=...` share link.
pub fn format_sticker_pack_url(pack_id: &str, pack_key: &str) -> String {
    format!(
        "https://signal.art/addstickers/#pack_id={}&pack_key={}",
        pack_id.to_ascii_lowercase(),
        pack_key.to_ascii_lowercase()
    )
}

/// Strictly parses an HTTPS Signal sticker pack link (`https://signal.art/addstickers/#pack_id=<32 hex>&pack_key=<64 hex>`).
/// Rejects non-HTTPS schemes, other hosts, and invalid hex lengths.
pub fn parse_sticker_pack_url(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim();
    let frag = trimmed
        .strip_prefix("https://signal.art/addstickers/#")
        .or_else(|| trimmed.strip_prefix("https://signal.art/addstickers#"))?;
    let mut pack_id: Option<String> = None;
    let mut pack_key: Option<String> = None;
    for pair in frag.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "pack_id" if is_hex_of_len(v, 32) => pack_id = Some(v.to_ascii_lowercase()),
                "pack_key" if is_hex_of_len(v, 64) => pack_key = Some(v.to_ascii_lowercase()),
                _ => {}
            }
        }
    }
    Some((pack_id?, pack_key?))
}

/// Finds the first valid `https://signal.art/addstickers/#...` link in `text`.
pub fn find_sticker_pack_url(text: &str) -> Option<(&str, String, String)> {
    let start = text.find("https://signal.art/addstickers")?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\''))
        .unwrap_or(rest.len());
    let candidate = rest[..end].trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | '!' | '?'));
    parse_sticker_pack_url(candidate).map(|(id, key)| (candidate, id, key))
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
    /// Initial list of installed Signal sticker packs loaded from the vault.
    StickerPacks(Vec<StickerPackInfo>),
    /// Another linked device installed or removed a sticker pack (`SyncMessage.stickerPackOperation`).
    StickerPackOp { pack_id: String, pack_key: String, install: bool },
    /// A Signal story (`ContentBody::StoryMessage`), lasting 24 hours from `sent`.
    Story(StoryMessageData),
    /// A reaction to a Signal story (`DataMessage.reaction` + `DataMessage.story_context`).
    StoryReaction { peer: Peer, by: String, story_author: String, story_sent: u64, emoji: String, removed: bool },
    /// Account-level story toggles (`stories_disabled`, `story_view_receipts`).
    StoriesSettings { disabled: bool, view_receipts: bool },
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
    /// True for recorded voice messages (`AttachmentPointer.Flags.VOICE_MESSAGE`).
    pub voice: bool,
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
    /// Send an encrypted Signal sticker from an installed pack.
    Sticker {
        pack_id: String,
        pack_key: String,
        sticker_id: u32,
        emoji: String,
        bytes: Vec<u8>,
        content_type: String,
        quote: Option<Quote>,
        timer: Timer,
    },
    /// Reply to a Signal story (`DataMessage.story_context`).
    StoryReply {
        story_author: String,
        story_sent: u64,
        text: String,
        ranges: Vec<Range>,
        timer: Timer,
    },
    /// React to a Signal story (`DataMessage.reaction` + `DataMessage.story_context`).
    StoryReact {
        story_author: String,
        story_sent: u64,
        emoji: String,
        remove: bool,
    },
    /// Post a new Signal story (`ContentBody::StoryMessage`) to resolved recipients or a group.
    PostStory {
        recipients: Vec<String>,
        group_id: Option<String>,
        text: Option<StoryText>,
        file: Option<File>,
        allows_replies: bool,
    },
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
    /// The user played/viewed these (author, sent) voice notes or stories: synced to our other
    /// devices, and with `receipts` sends `ReceiptMessage::Type::Viewed` to the authors too.
    fn viewed(&self, _viewed: &[(String, u64)], _receipts: bool) -> Result<(), Fault> {
        Ok(())
    }
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
    /// Streams an encrypted attachment from Signal's CDN, verifies its SHA-256 digest and
    /// HMAC-SHA256, and writes the decrypted plaintext to `dest` without buffering the whole
    /// file in memory. Returns the decrypted plaintext byte length.
    fn download_attachment(&self, _pointer: &Pointer, _dest: &std::path::Path) -> Result<u64, Fault> {
        Err(Fault::Refused("No longer available".to_owned()))
    }
    /// Fetches and decrypts a Signal sticker pack manifest without installing it.
    fn preview_sticker_pack(&self, _pack_id: &str, _pack_key: &str) -> Result<StickerPackInfo, Fault> {
        Err(Fault::Refused("Sticker packs are not available.".to_owned()))
    }
    /// Installs a Signal sticker pack locally and syncs the install operation to other devices.
    fn install_sticker_pack(&self, _pack_id: &str, _pack_key: &str) -> Result<StickerPackInfo, Fault> {
        Err(Fault::Refused("Sticker packs are not available.".to_owned()))
    }
    /// Removes an installed Signal sticker pack locally and syncs the removal to other devices.
    fn remove_sticker_pack(&self, _pack_id: &str, _pack_key: &str) -> Result<(), Fault> {
        Err(Fault::Refused("Sticker packs are not available.".to_owned()))
    }
    /// Downloads and decrypts one sticker image (`pack_id`, `pack_key`, `sticker_id`) into `dest`.
    fn download_sticker(
        &self,
        _pack_id: &str,
        _pack_key: &str,
        _sticker_id: u32,
        _dest: &std::path::Path,
    ) -> Result<u64, Fault> {
        Err(Fault::Refused("No longer available".to_owned()))
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

/// Progress reported while waiting for or downloading a linked-device history transfer archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferProgress {
    Waiting,
    Downloading { downloaded_bytes: u64, total_bytes: u64 },
}

/// Result of waiting for and downloading a linked-device history transfer archive.
#[derive(Debug)]
pub enum TransferOutcome {
    /// Encrypted archive downloaded to `path`, ready for streaming decryption and store import.
    Ready {
        path: std::path::PathBuf,
        ephemeral_key: [u8; 32],
        total_bytes: u64,
    },
    /// Primary phone declined or skipped transferring history.
    Declined(String),
    /// Transfer window or CDN archive expired (e.g. HTTP 404/410 or timeout).
    Expired(String),
    /// The user chose to skip the transfer.
    Skipped,
    /// Network or server error (the device remains linked).
    Failed(String),
}

/// Waits for the primary phone to upload a linked-device message history archive and streams the
/// encrypted archive to disk. Respects `skip` promptly so the user can skip at any time.
pub trait HistoryTransfer: Send {
    fn fetch(
        self: Box<Self>,
        on_progress: &dyn Fn(TransferProgress),
        skip: Arc<AtomicBool>,
    ) -> TransferOutcome;
}

/// What a finished link produced.
pub struct Linked {
    /// What the keychain keeps (`super::Saved`, as JSON).
    pub session: String,
    /// Optional post-link history transfer when the primary phone supplied an ephemeral backup key.
    pub transfer: Option<Box<dyn HistoryTransfer>>,
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
        assert!(story_preview_kept("See https://example.org/post for details", "https://example.org/post"));
        assert!(!story_preview_kept("See https://example.org/post for details", "https://evil.example.net/phish"));
        assert!(!story_preview_kept("See http://example.org/post for details", "http://example.org/post"));
        assert!(!story_preview_kept("No links here", "https://example.org/post"));
    }

    #[test]
    fn sticker_pack_url_strictly_validates_https_and_hex_lengths() {
        let pack_id = "0123456789abcdef0123456789abcdef";
        let pack_key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let url = format_sticker_pack_url(pack_id, pack_key);
        assert_eq!(parse_sticker_pack_url(&url), Some((pack_id.to_owned(), pack_key.to_owned())));
        // Reversed fragment order also parses.
        let reversed = format!("https://signal.art/addstickers/#pack_key={pack_key}&pack_id={pack_id}");
        assert_eq!(parse_sticker_pack_url(&reversed), Some((pack_id.to_owned(), pack_key.to_owned())));
        // Found inside a message sentence.
        let msg = format!("Check out this pack: {url}!");
        let (found, id, key) = find_sticker_pack_url(&msg).expect("sticker url");
        assert_eq!((found, id.as_str(), key.as_str()), (url.as_str(), pack_id, pack_key));
        // Non-HTTPS, wrong host, or bad hex lengths are rejected.
        assert!(parse_sticker_pack_url(&url.replace("https://", "http://")).is_none());
        assert!(parse_sticker_pack_url(&url.replace("signal.art", "evil.example")).is_none());
        assert!(parse_sticker_pack_url("https://signal.art/addstickers/#pack_id=1234&pack_key=abcd").is_none());
    }

    #[test]
    fn story_distribution_list_enforces_allowlist_blocklist_and_blocked_privacy() {
        let me = "0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b";
        let alice = "11111111-0000-4000-8000-000000000001";
        let bob = "22222222-0000-4000-8000-000000000002";
        let carol = "33333333-0000-4000-8000-000000000003";
        let dave = "44444444-0000-4000-8000-000000000004";
        let contacts = vec![me.to_owned(), alice.to_owned(), bob.to_owned(), carol.to_owned(), dave.to_owned()];
        let mut blocked = std::collections::HashSet::new();
        blocked.insert(dave.to_owned());

        // Blocklist mode ("My Story except Bob"): includes Alice and Carol, excludes Bob, blocked Dave, and self.
        let my_story = StoryDistributionList {
            id: "00000000000000000000000000000000".into(),
            name: "My Story".into(),
            recipients: vec![bob.to_owned()],
            allows_replies: true,
            is_block_list: true,
        };
        assert_eq!(
            resolve_story_recipients(&my_story, &contacts, &blocked, me),
            vec![alice.to_owned(), carol.to_owned()]
        );

        // Allowlist mode ("Close friends: Alice & Dave"): includes only Alice (Dave is blocked; Bob & Carol never receive it).
        let custom = StoryDistributionList {
            id: "abcdef0123456789abcdef0123456789".into(),
            name: "Close friends".into(),
            recipients: vec![alice.to_owned(), dave.to_owned(), alice.to_owned(), me.to_owned()],
            allows_replies: false,
            is_block_list: false,
        };
        assert_eq!(
            resolve_story_recipients(&custom, &contacts, &blocked, me),
            vec![alice.to_owned()]
        );
    }
}
