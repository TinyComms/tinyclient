//! The seam between the adapter and the MTProto library. Everything above it speaks Telegram's
//! own schema (`tl`): which requests to make and what their answers mean. Everything below it
//! is transport: keys, encryption, the socket, update gaps. `real` is grammers; `fake` is a
//! script, so the adapter can be run through chatd's socket with no Telegram behind it.

use grammers_tl_types as tl;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use tl::{Deserializable, RemoteCall};

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Telegram answered with an error: `FLOOD_WAIT` with `value` 31, `PHONE_CODE_INVALID`.
    Rpc { code: i32, name: String, value: Option<u32> },
    /// The request never got an answer.
    Net(String),
}

impl Fault {
    pub fn is(&self, name: &str) -> bool {
        matches!(self, Fault::Rpc { name: have, .. } if have == name)
    }

    /// The seconds Telegram said to wait before asking again.
    pub fn flood(&self) -> Option<u64> {
        match self {
            Fault::Rpc { code: 420, value: Some(seconds), .. } => Some(u64::from(*seconds)),
            _ => None,
        }
    }

    /// The session is over: signed out from another device, revoked, or the account is gone.
    pub fn signed_out(&self) -> bool {
        const ENDED: &[&str] = &[
            "AUTH_KEY_UNREGISTERED",
            "AUTH_KEY_INVALID",
            "SESSION_REVOKED",
            "SESSION_EXPIRED",
            "USER_DEACTIVATED",
            "USER_DEACTIVATED_BAN",
            "AUTH_KEY_PERM_EMPTY",
        ];
        // 406: the same key was used from two places at once, and Telegram voided it.
        matches!(self, Fault::Rpc { code: 401, name, .. } if ENDED.contains(&name.as_str())) || self.is("AUTH_KEY_DUPLICATED")
    }

    /// For the user. Telegram's own names are kept out of sight where there are words for them.
    pub fn words(&self) -> String {
        match self {
            Fault::Rpc { .. } if self.flood().is_some() => wait_words(self.flood().unwrap_or(0)),
            Fault::Rpc { name, .. } => match name.as_str() {
                "CHAT_WRITE_FORBIDDEN" | "CHAT_SEND_PLAIN_FORBIDDEN" => "You can't write in this chat.".to_owned(),
                "USER_IS_BLOCKED" | "YOU_BLOCKED_USER" => "You can't message this person.".to_owned(),
                "USER_BANNED_IN_CHANNEL" => "You are not allowed to post here.".to_owned(),
                "SLOWMODE_WAIT" => "Slow mode is on in this chat. Try again in a moment.".to_owned(),
                "MESSAGE_TOO_LONG" => "That message is too long for Telegram.".to_owned(),
                "MESSAGE_EDIT_TIME_EXPIRED" => "This message is too old to edit.".to_owned(),
                "MESSAGE_NOT_MODIFIED" => "Nothing was changed.".to_owned(),
                "MESSAGE_DELETE_FORBIDDEN" => "You can't delete this message.".to_owned(),
                "MESSAGE_ID_INVALID" => "That message is no longer there.".to_owned(),
                "REACTION_INVALID" | "REACTIONS_TOO_MANY" => "That reaction isn't allowed here.".to_owned(),
                "PEER_FLOOD" => "Telegram is limiting this account for now.".to_owned(),
                "PEER_ID_INVALID" | "CHANNEL_INVALID" | "CHANNEL_PRIVATE" | "USER_ID_INVALID" => {
                    "Telegram no longer lets this account reach that chat.".to_owned()
                }
                other => format!("Telegram refused this ({other})."),
            },
            Fault::Net(_) => "Telegram did not answer.".to_owned(),
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Rpc { code, name, value: Some(value) } => write!(f, "{code} {name} ({value})"),
            Fault::Rpc { code, name, value: None } => write!(f, "{code} {name}"),
            Fault::Net(why) => f.write_str(why),
        }
    }
}

/// How a flood wait is told: the whole wait, never a shorter one.
pub fn wait_words(seconds: u64) -> String {
    match seconds {
        0..=89 => format!("Telegram asked for a pause. Try again in {seconds} seconds."),
        90..=5_399 => format!("Telegram asked for a pause. Try again in {} minutes.", seconds.div_ceil(60)),
        _ => format!("Telegram asked for a pause. Try again in {} hours.", seconds.div_ceil(3_600)),
    }
}

/// Something Telegram sent unasked, or the end of the connection.
pub enum Event {
    /// One update, in order, with the people and chats its container named.
    Update {
        update: Box<tl::enums::Update>,
        users: Vec<tl::enums::User>,
        chats: Vec<tl::enums::Chat>,
    },
    /// The connection is gone for good; a new wire is needed.
    Closed(String),
}

/// One connection to Telegram under one session.
pub trait Wire: Send + Sync {
    /// One request, serialized, in the session's home datacenter or the one named. Blocks.
    fn invoke(&self, dc: Option<i32>, body: Vec<u8>) -> Result<Vec<u8>, Fault>;
    /// The events, once: whoever takes them is the only reader.
    fn events(&self) -> Option<Receiver<Event>>;
    /// The datacenter the account lives in.
    fn home(&self) -> i32;
    /// The account turned out to live elsewhere (a sign-in was redirected).
    fn set_home(&self, dc: i32);
    /// The key now belongs to this user: updates may be followed from here on.
    fn signed_in(&self, user: &tl::types::User);
    /// The session as it is kept in the keychain: the keys, the home datacenter, who it is.
    fn session(&self, user: i64, name: &str) -> String;
    /// True once since the last call if a key was made that `session` does not yet hold saved.
    fn unsaved(&self) -> bool;
    /// Ends the connection: the events end, and every later request fails.
    fn close(&self);
}

/// One request and its answer, typed.
pub fn call<R: RemoteCall>(wire: &dyn Wire, request: &R) -> Result<R::Return, Fault> {
    call_in(wire, None, request)
}

pub fn call_in<R: RemoteCall>(wire: &dyn Wire, dc: Option<i32>, request: &R) -> Result<R::Return, Fault> {
    let body = wire.invoke(dc, request.to_bytes())?;
    R::Return::from_bytes(&body).map_err(|err| Fault::Net(format!("unreadable answer: {err}")))
}

/// Waits at most this long for one flood wait before the one retry. A longer wait is told to
/// the user at once and nothing is retried; no wait is ever cut short.
pub const PATIENCE: u64 = 60;

/// A request with Telegram's flood waits honored: a wait up to [`PATIENCE`] is slept in full
/// (`pause` is the adapter's interruptible wait and answers true when it was stopped) and the
/// request made once more; a longer one is the answer.
pub fn patient<R: RemoteCall>(wire: &dyn Wire, pause: &dyn Fn(Duration) -> bool, request: &R) -> Result<R::Return, Fault> {
    match call(wire, request) {
        Err(fault) => match fault.flood() {
            Some(seconds) if seconds <= PATIENCE => {
                if pause(Duration::from_secs(seconds)) {
                    return Err(fault);
                }
                call(wire, request)
            }
            _ => Err(fault),
        },
        answered => answered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_wait_is_told_whole_and_an_ended_session_is_known() {
        let flood = Fault::Rpc { code: 420, name: "FLOOD_WAIT".into(), value: Some(31) };
        assert_eq!(flood.flood(), Some(31));
        assert_eq!(flood.words(), "Telegram asked for a pause. Try again in 31 seconds.");
        assert_eq!(wait_words(91), "Telegram asked for a pause. Try again in 2 minutes.");
        assert_eq!(wait_words(7_200), "Telegram asked for a pause. Try again in 2 hours.");
        assert!(Fault::Rpc { code: 401, name: "AUTH_KEY_UNREGISTERED".into(), value: None }.signed_out());
        // A password still owed is not an ended session.
        assert!(!Fault::Rpc { code: 401, name: "SESSION_PASSWORD_NEEDED".into(), value: None }.signed_out());
        assert!(!Fault::Net("timed out".into()).signed_out());
    }
}
