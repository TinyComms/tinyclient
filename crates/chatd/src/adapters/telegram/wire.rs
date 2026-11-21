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

    /// The seconds Telegram said to wait before asking again. Slow-mode waits carry code 420 too,
    /// so exclude them: a slow-mode wait belongs to the chat's cooldown, not a global flood retry.
    pub fn flood(&self) -> Option<u64> {
        match self {
            Fault::Rpc { code: 420, name, value: Some(seconds) } if name != "SLOWMODE_WAIT" => Some(u64::from(*seconds)),
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
            Fault::Rpc { name, value, .. } => match name.as_str() {
                "CHAT_WRITE_FORBIDDEN" | "CHAT_SEND_PLAIN_FORBIDDEN" => "You can't write in this chat.".to_owned(),
                "USER_IS_BLOCKED" | "YOU_BLOCKED_USER" => "You can't message this person.".to_owned(),
                "USER_BANNED_IN_CHANNEL" => "You are not allowed to post here.".to_owned(),
                "SLOWMODE_WAIT" => match value {
                    Some(secs) if *secs >= 90 => format!("Slow mode is on in this chat. Try again in {} minutes.", secs.div_ceil(60)),
                    Some(secs) if *secs > 0 => format!("Slow mode is on in this chat. Try again in {secs} seconds."),
                    _ => "Slow mode is on in this chat. Try again in a moment.".to_owned(),
                },
                "CHAT_SEND_MEDIA_FORBIDDEN" => "Admins don't allow media in this chat.".to_owned(),
                "CHAT_SEND_PHOTOS_FORBIDDEN" => "Admins don't allow photos in this chat.".to_owned(),
                "CHAT_SEND_VIDEOS_FORBIDDEN" => "Admins don't allow videos in this chat.".to_owned(),
                "CHAT_SEND_ROUNDVIDEOS_FORBIDDEN" => "Admins don't allow video messages in this chat.".to_owned(),
                "CHAT_SEND_AUDIOS_FORBIDDEN" => "Admins don't allow audio files in this chat.".to_owned(),
                "CHAT_SEND_VOICES_FORBIDDEN" => "Admins don't allow voice messages in this chat.".to_owned(),
                "CHAT_SEND_DOCS_FORBIDDEN" => "Admins don't allow files in this chat.".to_owned(),
                "CHAT_SEND_STICKERS_FORBIDDEN" => "Admins don't allow stickers in this chat.".to_owned(),
                "CHAT_SEND_GIFS_FORBIDDEN" => "Admins don't allow GIFs in this chat.".to_owned(),
                "CHAT_SEND_POLL_FORBIDDEN" => "Admins don't allow polls in this chat.".to_owned(),
                "CHAT_SEND_INLINE_FORBIDDEN" => "Admins don't allow inline bots in this chat.".to_owned(),
                "CHAT_SEND_WEBPAGE_FORBIDDEN" => "Admins don't allow link previews in this chat.".to_owned(),
                "ALLOW_PAYMENT_REQUIRED" => match value {
                    Some(stars) if *stars > 0 => format!("Sending a message here costs {stars} Telegram Stars."),
                    _ => "Sending a message here requires Telegram Stars.".to_owned(),
                },
                "MESSAGE_TOO_LONG" => "That message is too long for Telegram.".to_owned(),
                "MESSAGE_EDIT_TIME_EXPIRED" => "This message is too old to edit.".to_owned(),
                "MESSAGE_NOT_MODIFIED" => "Nothing was changed.".to_owned(),
                "CHAT_FORWARDS_RESTRICTED" | "MESSAGE_FORWARDS_RESTRICTED" => "Forwarding is disabled in this chat.".to_owned(),
                "MESSAGE_DELETE_FORBIDDEN" => "You can't delete this message.".to_owned(),
                "MESSAGE_ID_INVALID" => "That message is no longer there.".to_owned(),
                "REACTION_INVALID" | "REACTIONS_TOO_MANY" => "That reaction isn't allowed here.".to_owned(),
                "SERVICE_NEVER_MUTED" => "Telegram's own account isn't muted from here.".to_owned(),
                "PEER_NOT_A_GROUP" => "Only a group or a channel can be left.".to_owned(),
                "PICK_EXPIRED" => "That sticker or GIF is no longer in the picker. Choose it again.".to_owned(),
                "PEER_FLOOD" => "Telegram is limiting this account for now.".to_owned(),
                "PEER_ID_INVALID" | "CHANNEL_INVALID" | "CHANNEL_PRIVATE" | "USER_ID_INVALID" => {
                    "Telegram no longer lets this account reach that chat.".to_owned()
                }
                "CHAT_ADMIN_REQUIRED" | "RIGHT_FORBIDDEN" => "You don't have admin permission to do that here.".to_owned(),
                "USER_CREATOR" => "You can't modify the group or channel creator.".to_owned(),
                "ADMINS_TOO_MUCH" => "This group or channel already has the maximum number of admins.".to_owned(),
                "PASSWORD_HASH_INVALID" => "That two-step verification password is incorrect.".to_owned(),
                "PASSWORD_REQUIRED" => "Two-step verification must be enabled on your Telegram account to transfer ownership.".to_owned(),
                "FRESH_CHANGE_ADMINS_FORBIDDEN" => "Telegram requires a newer session to transfer ownership. Try again later.".to_owned(),
                "CHAT_NOT_MODIFIED" => "Nothing was changed.".to_owned(),
                "SEND_AS_PEER_INVALID" => "You can't send messages as that peer here.".to_owned(),
                "CHANNEL_ONLY" => "That setting is only available in a channel or supergroup.".to_owned(),
                "PARTICIPANTS_HIDDEN" => "This group hides its member list.".to_owned(),
                "USER_PRIVACY_RESTRICTED" => "This person's privacy settings don't allow calls.".to_owned(),
                "CALL_PROTOCOL_COMPAT_LAYER_INVALID" => "This call protocol version isn't supported.".to_owned(),
                "PARTICIPANT_VERSION_OUTDATED" => "The other person's Telegram app needs an update for this call.".to_owned(),
                "PREMIUM_ACCOUNT_REQUIRED" => "This needs Telegram Premium.".to_owned(),
                // A length or a count past the free plan. `MESSAGE_TOO_LONG` and
                // `REACTIONS_TOO_MANY` are named above and are not this.
                name if name.ends_with("_TOO_MUCH") => "This needs Telegram Premium.".to_owned(),
                "SECRET_CHAT" => super::secret::PHONE.to_owned(),
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
    /// Whether the update stream may catch up on this channel on its own.
    fn channel_membership(&self, _id: i64, _member: bool) {}
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

    #[test]
    fn a_premium_limit_says_it_needs_telegram_premium() {
        let required = Fault::Rpc { code: 403, name: "PREMIUM_ACCOUNT_REQUIRED".into(), value: None };
        assert!(required.words().contains("needs Telegram Premium"), "{}", required.words());
        let too_much = Fault::Rpc { code: 400, name: "STICKERS_TOO_MUCH".into(), value: None };
        assert!(too_much.words().contains("needs Telegram Premium"), "{}", too_much.words());
        let long = Fault::Rpc { code: 400, name: "MESSAGE_TOO_LONG".into(), value: None };
        assert!(!long.words().contains("needs Telegram Premium"), "{}", long.words());
        let many = Fault::Rpc { code: 400, name: "REACTIONS_TOO_MANY".into(), value: None };
        assert!(!many.words().contains("needs Telegram Premium"), "{}", many.words());
    }

    #[test]
    fn a_secret_chat_says_it_only_works_in_the_phone_apps() {
        let fault = Fault::Rpc { code: 400, name: "SECRET_CHAT".into(), value: None };
        assert!(fault.words().contains("phone apps"), "{}", fault.words());
    }

    #[test]
    fn call_refusal_rpc_errors_map_to_plain_words() {
        let privacy = Fault::Rpc { code: 403, name: "USER_PRIVACY_RESTRICTED".into(), value: None };
        assert!(privacy.words().contains("privacy"), "{}", privacy.words());
        let compat = Fault::Rpc { code: 400, name: "CALL_PROTOCOL_COMPAT_LAYER_INVALID".into(), value: None };
        assert!(compat.words().contains("protocol"), "{}", compat.words());
        let outdated = Fault::Rpc { code: 400, name: "PARTICIPANT_VERSION_OUTDATED".into(), value: None };
        assert!(outdated.words().contains("update"), "{}", outdated.words());
    }

    #[test]
    fn slowmode_media_and_paid_errors_map_to_plain_words() {
        let slow = Fault::Rpc { code: 420, name: "SLOWMODE_WAIT".into(), value: Some(25) };
        assert_eq!(slow.flood(), None, "slow mode is not a global flood wait");
        assert!(slow.words().contains("25 seconds"), "{}", slow.words());
        let media = Fault::Rpc { code: 403, name: "CHAT_SEND_MEDIA_FORBIDDEN".into(), value: None };
        assert!(media.words().contains("media"), "{}", media.words());
        let stickers = Fault::Rpc { code: 403, name: "CHAT_SEND_STICKERS_FORBIDDEN".into(), value: None };
        assert!(stickers.words().contains("stickers"), "{}", stickers.words());
        let paid = Fault::Rpc { code: 403, name: "ALLOW_PAYMENT_REQUIRED".into(), value: Some(50) };
        assert!(paid.words().contains("50 Telegram Stars"), "{}", paid.words());
    }
}
