//! A Tiny server's members, invites, audit, rooms, text channels, and direct messages. A room is a voice channel:
//! joining takes a seat in its call, with audio when the server's audio is up.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyMember {
    pub user_id: String,
    /// Empty when the server has none for them.
    #[serde(default, deserialize_with = "text_or_null")]
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyInvite {
    pub id: String,
    pub role: String,
    pub created_by: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_by: Option<String>,
}

/// The token is only in the answer to creating an invite.
/// `origin` is the signed-in server, filled in by chatd. The server does not send it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyCode {
    pub id: String,
    pub token: String,
    pub role: String,
    pub expires_at: String,
    #[serde(default)]
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyAudit {
    pub id: i64,
    pub actor_id: Option<String>,
    pub action: String,
    pub subject: String,
    /// The server's detail object, as JSON text. Empty when the server sent none.
    pub detail: String,
    pub created_at: String,
}

/// One room, and who is in its call now, in the order they joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyRoom {
    pub id: String,
    pub name: String,
    /// Filled in by chatd from the live socket; the server's room list has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<TinySeat>,
}

/// Someone in a room's call, and whether they muted or deafened themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinySeat {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deafened: bool,
    /// Heard right now. Only known while we are in the same call.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub speaking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyCommunity {
    pub role: String,
    pub members: Vec<TinyMember>,
    pub invites: Vec<TinyInvite>,
    pub audit: Vec<TinyAudit>,
    /// The signed-in server, filled in by chatd. Empty when an older answer had none.
    #[serde(default)]
    pub origin: String,
    /// The server's rooms, in its order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rooms: Vec<TinyRoom>,
    /// Members online now, as the live socket last said. Empty while it is not connected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub online: Vec<String>,
    /// The server's audio, as its `/health` says: `off`, `up` or `down`. Empty when it did not
    /// say. With `up` a join carries audio; with `off` or empty it takes a seat alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub media: String,
}

/// The most characters in a display name, after trimming.
pub const TINY_NAME_MAX: usize = 80;

/// A string the server may send as `null`: someone whose provider gave no email has none.
pub fn text_or_null<'de, D: serde::Deserializer<'de>>(from: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(from)?.unwrap_or_default())
}

/// The name the server would keep, or `None` when it would refuse it.
pub fn tiny_name(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= TINY_NAME_MAX).then_some(name)
}

/// One paste for the other tinyclient: `tc1.` then the origin in hex, then the server's token.
/// The server never sees this string. Signing in still sends only `token`, as the invite.
pub fn pack_join(origin: &str, token: &str) -> Option<String> {
    if origin.is_empty() || token.is_empty() {
        return None;
    }
    if origin.chars().any(char::is_whitespace) || token.chars().any(|c| c.is_whitespace() || c == '.') {
        return None;
    }
    let hex: String = origin.bytes().map(|byte| format!("{byte:02x}")).collect();
    Some(format!("tc1.{hex}.{token}"))
}

/// The origin and the server's token inside a pasted code. Whitespace is ignored.
/// `None` when it is not this code: a raw token, a typed address, or a damaged paste.
pub fn unpack_join(pasted: &str) -> Option<(String, String)> {
    let compact: String = pasted.chars().filter(|c| !c.is_whitespace()).collect();
    let rest = compact.strip_prefix("tc1.")?;
    let (hex, token) = rest.rsplit_once('.')?;
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.bytes().all(|byte| from_hex(byte).is_some()) {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let hex = hex.as_bytes();
    let mut at = 0;
    while at < hex.len() {
        let high = from_hex(hex[at])?;
        let low = from_hex(hex[at + 1])?;
        bytes.push((high << 4) | low);
        at += 2;
    }
    let origin = String::from_utf8(bytes).ok()?;
    if origin.is_empty() || origin.chars().any(char::is_whitespace) {
        return None;
    }
    Some((origin, token.to_owned()))
}

/// The host, and a port when it is not the usual one. `https://private.tinyclient.io` is
/// `private.tinyclient.io`. `http://127.0.0.1:8080` keeps the port.
pub fn join_host(origin: &str) -> String {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return origin.to_owned();
    };
    if rest.is_empty() {
        return origin.to_owned();
    }
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        match v6.split_once(']') {
            Some((addr, after)) => (format!("[{addr}]"), after.strip_prefix(':').unwrap_or("")),
            None => return rest.to_owned(),
        }
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) => (host.to_owned(), port),
            _ => (rest.to_owned(), ""),
        }
    };
    let usual = (scheme == "https" && port == "443") || (scheme == "http" && port == "80");
    if port.is_empty() || usual {
        host
    } else {
        format!("{host}:{port}")
    }
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether `role` may do `id`. An unknown id is refused.
pub fn tiny_can(role: &str, id: &str) -> bool {
    match id {
        "community.members.read" => matches!(role, "owner" | "admin" | "member"),
        "community.invites.read"
        | "community.invites.create"
        | "community.members.role"
        | "community.members.remove"
        | "community.invites.revoke"
        | "community.audit.read" => matches!(role, "owner" | "admin"),
        "account.name.edit" | "account.sessions.end" | "community.calls.join" => {
            matches!(role, "owner" | "admin" | "member")
        }
        "community.rooms.create" | "community.rooms.rename" | "community.rooms.delete" | "community.calls.kick" => {
            matches!(role, "owner" | "admin")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_can_matrix() {
        assert!(tiny_can("member", "community.members.read"));
        assert!(tiny_can("admin", "community.members.read"));
        assert!(tiny_can("owner", "community.members.read"));
        assert!(!tiny_can("guest", "community.members.read"));
        for id in [
            "community.invites.read",
            "community.invites.create",
            "community.invites.revoke",
            "community.members.role",
            "community.members.remove",
            "community.audit.read",
            "community.rooms.create",
            "community.rooms.rename",
            "community.rooms.delete",
            "community.calls.kick",
        ] {
            assert!(!tiny_can("member", id), "{id}");
            assert!(tiny_can("admin", id), "{id}");
            assert!(tiny_can("owner", id), "{id}");
            assert!(!tiny_can("someone", id), "{id}");
        }
        assert!(!tiny_can("owner", "community.nope"));
        assert!(!tiny_can("admin", ""));
        assert!(!tiny_can("member", "community.invites.create"));
        for id in ["account.name.edit", "account.sessions.end", "community.calls.join"] {
            for role in ["owner", "admin", "member"] {
                assert!(tiny_can(role, id), "{role} {id}");
            }
            assert!(!tiny_can("guest", id), "{id}");
        }
    }

    #[test]
    fn a_name_is_trimmed_and_an_email_may_be_null() {
        assert_eq!(tiny_name("  Ada  "), Some("Ada"));
        assert_eq!(tiny_name("   "), None);
        assert!(tiny_name(&"é".repeat(80)).is_some());
        assert_eq!(tiny_name(&"a".repeat(81)), None);
        let member: TinyMember = serde_json::from_str(
            r#"{"user_id":"u","email":null,"display_name":"Ada","role":"member","created_at":"t"}"#,
        )
        .unwrap();
        assert_eq!(member.email, "");
        let member: TinyMember = serde_json::from_str(r#"{"user_id":"u","display_name":"Ada","role":"member","created_at":"t"}"#).unwrap();
        assert_eq!(member.email, "");
    }

    #[test]
    fn a_join_code_carries_the_server_and_an_older_answer_still_reads() {
        let origin = "https://private.tinyclient.io";
        let token = "abcdefghijklmnopqrstuvwxyz0123456789-_ABC";
        let packed = pack_join(origin, token).unwrap();
        assert!(packed.starts_with("tc1."));
        assert!(!packed.contains(origin));
        assert_eq!(unpack_join(&packed), Some((origin.to_owned(), token.to_owned())));
        let wrapped = format!("  {}\n{}", &packed[..20], &packed[20..]);
        assert_eq!(unpack_join(&wrapped), Some((origin.to_owned(), token.to_owned())));
        assert_eq!(unpack_join(token), None);
        assert_eq!(unpack_join("https://private.tinyclient.io"), None);
        assert_eq!(pack_join("", token), None);
        assert_eq!(pack_join(origin, "has.dot"), None);
        assert!(pack_join("http://127.0.0.1:8080", token).is_some());
        assert_eq!(join_host(origin), "private.tinyclient.io");
        assert_eq!(join_host("https://private.tinyclient.io:443"), "private.tinyclient.io");
        assert_eq!(join_host("http://127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(join_host("http://127.0.0.1:80"), "127.0.0.1");
        assert_eq!(join_host("http://[::1]:8080"), "[::1]:8080");
        assert_eq!(join_host("https://chat.example:8443"), "chat.example:8443");
        let old: TinyCode = serde_json::from_str(r#"{"id":"1","token":"t","role":"member","expires_at":"t"}"#).unwrap();
        assert_eq!(old.origin, "");
        let community: TinyCommunity = serde_json::from_str(r#"{"role":"owner","members":[],"invites":[],"audit":[]}"#).unwrap();
        assert_eq!(community.origin, "");
        assert!(community.rooms.is_empty() && community.online.is_empty() && community.media.is_empty());
        // Nothing new is written while it is empty, so an older shell reads the same frame.
        let json = serde_json::to_string(&community).unwrap();
        assert!(!json.contains("rooms") && !json.contains("online") && !json.contains("media"), "{json}");
        let room: TinyRoom = serde_json::from_str(r#"{"id":"r","name":"Lobby"}"#).unwrap();
        assert!(room.participants.is_empty());
        assert!(!serde_json::to_string(&room).unwrap().contains("participants"));
    }

    #[test]
    fn an_invite_omits_an_acceptance_it_does_not_have() {
        let invite = TinyInvite {
            id: "1".into(),
            role: "member".into(),
            created_by: "a".into(),
            expires_at: "t".into(),
            accepted_at: None,
            accepted_by: None,
        };
        let json = serde_json::to_string(&invite).unwrap();
        assert!(!json.contains("accepted"));
        let back: TinyInvite = serde_json::from_str(r#"{"id":"1","role":"member","created_by":"a","expires_at":"t"}"#).unwrap();
        assert_eq!(back, invite);
    }
}
