//! End-to-end encrypted text on a Tiny server, as the shell sees it: whether a conversation is
//! encrypted, whether a message could be opened, and this account's devices, the people it shares
//! groups with, their safety numbers and the recovery key. chatd's `adapters::tiny::e2ee` holds
//! the keys; nothing here carries one.

use serde::{Deserialize, Serialize};

use crate::Trust;

/// Whether a conversation is end-to-end encrypted, as its network says. `Unknown` for every
/// network that does not say; the shell draws nothing for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encryption {
    #[default]
    Unknown,
    /// The server can read it. It stays this way until a member upgrades it.
    Plain,
    /// Only the members' devices hold the keys, including everything already in it.
    EndToEnd,
    /// End-to-end encrypted from an upgrade on. Messages and files from before that stay where
    /// the server can read them.
    EndToEndAfterPlain,
}

impl Encryption {
    pub fn is_unknown(&self) -> bool {
        *self == Encryption::Unknown
    }

    /// The server cannot read new messages. An upgrade that left older messages in the clear
    /// still counts: those stay labelled on their own.
    pub fn is_sealed(self) -> bool {
        matches!(self, Encryption::EndToEnd | Encryption::EndToEndAfterPlain)
    }

    /// The words the conversation's header says.
    pub fn words(self) -> &'static str {
        match self {
            Encryption::Unknown => "",
            Encryption::Plain => "Not end-to-end encrypted",
            Encryption::EndToEnd | Encryption::EndToEndAfterPlain => "End-to-end encrypted",
        }
    }

    /// The longer sentence, for a tooltip or the details.
    pub fn explained(self) -> &'static str {
        match self {
            Encryption::Unknown => "",
            Encryption::Plain => "Not end-to-end encrypted: the server can read this conversation.",
            Encryption::EndToEnd => {
                "End-to-end encrypted: only the members' devices can read it, not the server."
            }
            Encryption::EndToEndAfterPlain => {
                "End-to-end encrypted from now on. Messages and files sent before this chat was encrypted can still be read by the server."
            }
        }
    }
}

/// What one message is, as far as encryption goes. `Plain` for every message of every network
/// that does not encrypt end to end, and for a Tiny conversation's plaintext messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seal {
    #[default]
    Plain,
    /// Opened on this device from end-to-end encrypted ciphertext (or sent from it).
    Encrypted,
    /// End-to-end encrypted, and this device cannot open it: sent before it joined, or for keys it
    /// no longer has. The message's text is [`UNDECRYPTABLE`] so nothing ever shows a blank.
    Undecryptable,
}

impl Seal {
    pub fn is_plain(&self) -> bool {
        *self == Seal::Plain
    }
}

/// The text of a message this device cannot open.
pub const UNDECRYPTABLE: &str = "Can't be decrypted on this device";
/// What a new member sees above the first message they can read.
pub const BEFORE_JOINING: &str = "Messages from before you joined can't be read.";

/// The 60-digit safety number, as 12 groups of five.
pub fn grouped_safety(digits: &str) -> String {
    let digits: Vec<char> = digits.chars().filter(char::is_ascii_digit).collect();
    digits
        .chunks(5)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Concrete glyphs for a digit code (call code or safety number), Telegram-style: no skin
/// tones, no lookalike pairs. Same digits always map to the same four.
const SAFETY_EMOJIS: &[&str] = &[
    "🦊", "🎩", "🌙", "🎸", "🍎", "⚡", "🎯", "🌵", "🎲", "🌈", "🚀", "🍀", "🎧", "🦄", "🍕", "🔑",
    "🌊", "🎨", "🐸", "🍩", "🔥", "🎈", "🐣", "🌹", "🐯", "🎵", "🌻", "🪐", "🫐", "🪼", "🍉", "🧿",
    "🚲", "🍋", "🦋", "🍄", "🪁", "🐙", "🌽", "💎", "🦉", "🥐", "🧲", "🐝", "🍇", "🧭", "🦜", "🍓",
    "🥁", "🐨", "🌶", "🧊", "🦚", "🧀", "🐢", "🦩", "🥯", "🦞", "🌼", "🧶", "🫖", "🛵", "🧈", "🪺",
];

/// Four emojis from a digit string (call code or pairwise safety number). Empty or non-digit
/// input yields none. Everyone who holds the same digits sees the same four.
pub fn safety_emojis(digits: &str) -> Vec<&'static str> {
    let digits: Vec<u8> = digits.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    if digits.is_empty() {
        return Vec::new();
    }
    (0..4)
        .map(|i| {
            let mut n = 0u32;
            for (j, d) in digits.iter().enumerate() {
                n = n.wrapping_mul(10).wrapping_add(u32::from(*d));
                n = n.wrapping_add(((i as u32) + 1).wrapping_mul((j as u32) + 1).wrapping_mul(17));
            }
            n = n.wrapping_mul(0x9E37_79B1).wrapping_add((i as u32).wrapping_mul(0x85EB_CA6B));
            SAFETY_EMOJIS[(n as usize) % SAFETY_EMOJIS.len()]
        })
        .collect()
}

/// What `Request::TinyE2ee` asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum E2eeAsk {
    /// This device, this person's devices and the people met in encrypted conversations.
    /// Answers `TinyE2ee`.
    Status,
    /// One person's devices and the safety number with them, fetched now. Answers `TinyE2eePerson`.
    Person { user_id: String },
    /// Approves a new device of this person from the link code it shows: this device signs it
    /// and registers it. Answers `Ok`.
    Approve { code: String },
    /// Revokes one of this person's devices (this one included). Answers `Ok`.
    Revoke { device_id: String },
    /// Marks the safety number with someone as compared, or takes that back. Answers `Ok`.
    Verify { user_id: String, verified: bool },
    /// The person agrees that someone has a new identity (every device of theirs was replaced).
    /// Answers `Ok`.
    AcceptIdentity { user_id: String },
    /// Makes a recovery key and answers it once, in `TinyRecoveryKey`. Making another replaces it.
    RecoveryKey,
    /// Checks a recovery key the person typed. Answers `Ok`, or an error that says it does not match.
    CheckRecovery { key: String },
    /// On a device waiting to be approved: makes a link and shows its code, then waits for a
    /// device the person has to answer. Answers `Ok`; what follows is in `Link`, and each step
    /// is a `Change::Tiny`.
    LinkStart,
    /// On a device the person has: the code a new device shows, pasted or scanned. A
    /// `tiny-device-` code (from a device on a server without device linking) is approved the old
    /// way. Answers `Ok`.
    LinkJoin { code: String },
    /// On the device the person has: they checked the new device and typed the six digits it
    /// shows. `history` is `none`, `days:N` or `all`. Answers `Ok`, or an error when the digits
    /// do not match (the link stops).
    LinkConfirm { history: String, check: String },
    /// Stops the link on this device and tells the other one. Answers `Ok`.
    LinkCancel,
    /// The link in progress, without asking the server. Answers `TinyE2eeLink`.
    Link,
    /// The person knows a new device of theirs ("New device added"): the notice goes. Answers `Ok`.
    SeenDevice { device_id: String },
}

/// Where a device link stands, on either device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStage {
    /// The new device shows its code and waits for a device the person has to take it.
    #[default]
    Waiting,
    /// Both devices show the same six digits. The old one asks the person to confirm.
    Confirm,
    /// The old device is signing the new one and packing up history.
    Sending,
    /// The new device is registering and unpacking history.
    Receiving,
    Done,
    /// Stopped: `error` says why, in words.
    Failed,
}

/// The device link in progress on this device (`E2eeAsk::Link`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eeLink {
    /// `new` on the device being added, `old` on the device approving it.
    pub role: String,
    pub stage: LinkStage,
    /// On the new device: the code to show, as text and as a QR of the same string.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// When the code runs out (the server's `expires_at`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expires_at: String,
    /// Six digits, once both devices have them. The new device shows them; the old one asks for
    /// them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub check: String,
    /// On the old device: what the new device says it is, and its key's fingerprint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub os: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    /// On the old device, at `Confirm`: how much history each choice would send.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<E2eeHistory>,
    /// Bytes of history moved so far, of `total`.
    #[serde(default, skip_serializing_if = "crate::is_zero64")]
    pub done: u64,
    #[serde(default, skip_serializing_if = "crate::is_zero64")]
    pub total: u64,
    /// Messages the new device took in, at `Done`.
    #[serde(default, skip_serializing_if = "crate::is_zero64")]
    pub messages: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// One choice of how much history to send, with its size.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eeHistory {
    /// `none`, `days:N` or `all`.
    pub history: String,
    pub messages: u64,
    /// About what it takes to send.
    pub bytes: u64,
}

/// Where this device stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceState {
    /// The server does not speak encrypted text, or the account has not started yet.
    #[default]
    Off,
    /// Registered, with key packages up: it can join and send.
    Ready,
    /// This person already has devices; one of them must approve this one. `code` is what to
    /// paste there.
    AwaitingApproval { code: String },
    /// Something went wrong, in words.
    Failed { why: String },
}

/// One device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eeDevice {
    pub device_id: String,
    /// The label its owner gave it. Only a person's own devices have one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// The server's `created_at`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
    /// The first bytes of SHA-256 over its signing key, in hex groups: what two screens compare.
    pub fingerprint: String,
    /// This device.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub this: bool,
    /// On the server's list, and not chained by cross-signatures to a device this client trusts.
    /// The server may have added it. Never used for encryption.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub untrusted: bool,
}

/// Someone this account shares encrypted conversations with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eePerson {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// 60 digits. Empty when no device of theirs is trusted yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub safety: String,
    pub devices: Vec<E2eeDevice>,
    /// Compared, and still the number that was compared; or compared once and changed since.
    #[serde(default)]
    pub trust: Trust,
    /// None of their devices chain to the ones pinned before: they set tinyclient up again, or
    /// someone (the server among them) put new keys in their place. Their new devices are let into
    /// no group until the person accepts it.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub identity_changed: bool,
}

/// `TinyE2ee`'s answer to `Status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eeStatus {
    /// The server speaks encrypted text (`e2ee_text`), and what it lets new conversations be:
    /// `off`, `optional` or `new_required`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(default)]
    pub device: DeviceState,
    /// This device's id and fingerprint, once made.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    /// This person's devices on the server, this one among them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<E2eeDevice>,
    /// The people met in encrypted conversations, with what is known of their devices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<E2eePerson>,
    /// A recovery key was made on this device.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub recovery_set: bool,
    /// Key backup is not built yet: the recovery key is shown and checked, and backs nothing up.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub backup: bool,
    /// The server relays device links (`e2ee_device_link`): a new device is added with a code
    /// and gets its history. Without it, the paste-the-code approval is all there is.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub linking: bool,
    /// The person's devices that appeared since they last looked: "New device added".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<E2eeDevice>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_safety_number_reads_in_groups_of_five() {
        let digits: String = (0..60).map(|n| char::from(b'0' + (n % 10) as u8)).collect();
        let grouped = grouped_safety(&digits);
        assert_eq!(grouped.split(' ').count(), 12);
        assert!(grouped.starts_with("01234 56789 "));
    }

    #[test]
    fn safety_emojis_are_four_stable_glyphs_from_the_digits() {
        assert!(safety_emojis("").is_empty());
        assert!(safety_emojis("abc").is_empty());
        let one = safety_emojis("686286309597067940951489038115");
        assert_eq!(one.len(), 4);
        assert_eq!(one, safety_emojis("686286309597067940951489038115"));
        assert_ne!(one, safety_emojis("686286309597067940951489038116"));
        for glyph in &one {
            assert!(SAFETY_EMOJIS.contains(glyph));
        }
    }

    #[test]
    fn the_defaults_say_nothing_and_old_plaintext_is_never_called_encrypted() {
        assert!(Encryption::default().is_unknown());
        assert!(Seal::default().is_plain());
        assert_eq!(Encryption::Plain.words(), "Not end-to-end encrypted");
        assert!(Encryption::EndToEnd.is_sealed());
        assert!(Encryption::EndToEndAfterPlain.is_sealed());
        assert!(!Encryption::Plain.is_sealed());
        assert_eq!(Encryption::EndToEndAfterPlain.words(), "End-to-end encrypted");
        assert!(Encryption::EndToEndAfterPlain.explained().contains("before this chat"));
        let ask: E2eeAsk =
            serde_json::from_str(r#"{"op":"verify","user_id":"u","verified":true}"#).unwrap();
        assert_eq!(
            ask,
            E2eeAsk::Verify {
                user_id: "u".into(),
                verified: true
            }
        );
    }
}
