//! The settings chatd keeps (`docs/product/09-settings.md`): their names, scopes, defaults and
//! the values each may take. chatd stores only what differs from the default; a missing row is
//! the default, so a better default reaches everyone who never touched the control.
//!
//! Values are text: `on` / `off` for switches, decimal for numbers, a device's system name for
//! devices. A stored value that does not parse reads as the default.

use crate::{AccountKey, Key, Network};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One stored setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    /// `""` global, `account:<network>:<id>`, or `conv:<network>:<account>:<id>`.
    pub scope: String,
    pub key: String,
    pub value: String,
}

/// The global scope.
pub const GLOBAL: &str = "";
/// Operator ceiling for this machine's logs: `error`, `warn`, `info`, `debug`, or `trace`.
pub const LOG_LEVEL: &str = "log_level";
pub const LOG_LEVEL_CHOICES: [&str; 5] = ["error", "warn", "info", "debug", "trace"];
/// Whether the in-memory debug ring is kept.
pub const LOG_RING: &str = "log_ring";
/// Whether Save debug log may write that ring to disk.
pub const LOG_EXPORT: &str = "log_export";

/// G2. Closing the window leaves chatd running. Read by the shell.
pub const KEEP_RUNNING: &str = "keep_running";
/// S1. Off: calls make no sounds.
pub const SOUNDS: &str = "sounds";
/// S7, S8, S9: which in-call cues play.
pub const CUE_JOINS: &str = "cue_joins";
pub const CUE_MINE: &str = "cue_mine";
pub const CUE_CONNECTION: &str = "cue_connection";
/// S10. The cues' volume, 0 to 100.
pub const CUE_VOLUME: &str = "cue_volume";
/// V1, V2. The microphone and the speaker, as [`DeviceChoice::value`] writes them: absent is
/// [`DeviceChoice::Auto`], [`SYSTEM_DEFAULT`] follows the system default, anything else is one
/// device by name, then a tab and the system's id for it. A value with no tab is a name alone,
/// as builds before 0.2.20 stored it; chatd adds the id once the device is listed
/// ([`DeviceChoice::upgrade`]).
pub const INPUT: &str = "input";
pub const OUTPUT: &str = "output";
/// The stored value of "System default" in V1 and V2.
pub const SYSTEM_DEFAULT: &str = "@system";
/// V4. Every join and every Accept starts muted.
pub const JOIN_MUTED: &str = "join_muted";
/// V19: "You're muted" shows when the microphone hears speech while I am muted in a call.
pub const MUTED_TALK: &str = "muted_talk";
/// Only join Tiny calls that are end-to-end encrypted. On: a call whose server turns encryption off
/// stops sending and playing (mediad `Blocked`), and a server that cannot encrypt is not joined.
/// Off by default on purpose: an older server or an older app in the room cannot do it, and
/// turning it on by default would stop those calls working. See `SECURITY.md`.
pub const REQUIRE_CALL_E2EE: &str = "require_call_e2ee";
/// V5. Everyone's voice, 0 to 100.
pub const VOICE_VOLUME: &str = "voice_volume";
/// V6. The level, 0 to 100, from which our microphone counts as speech, and in voice
/// activation opens.
pub const SENSITIVITY: &str = "sensitivity";
/// V8. On: the microphone opens only while the push-to-talk key is held.
pub const PUSH_TO_TALK: &str = "push_to_talk";
/// V8. How long the microphone stays open after push-to-talk is let go, in milliseconds.
pub const PTT_RELEASE_MS: &str = "ptt_release_ms";
/// V10, V11, V12. The microphone's processing.
pub const ECHO_CANCELLATION: &str = "echo_cancellation";
pub const NOISE_SUPPRESSION: &str = "noise_suppression";
pub const AUTO_GAIN: &str = "auto_gain";
/// V8, V13, V14, V15. The shell's global keys, as [`crate::hotkey::Binding`] writes them:
/// push-to-talk, push-to-talk with priority, toggle mute, toggle deafen. Absent is no key.
/// Read by the shell; chatd only keeps them.
pub const PTT_KEY: &str = "ptt_key";
pub const PRIORITY_KEY: &str = "priority_key";
pub const MUTE_KEY: &str = "mute_key";
pub const DEAFEN_KEY: &str = "deafen_key";
/// V9, per account: one person's volume for me, keyed `gain:<their user id>` ([`gain_key`]).
/// The value is the percent, 0 to [`crate::GAIN_MAX`], then ` muted` when I muted them; 100
/// and heard is the default and is not stored.
pub const GAIN_PREFIX: &str = "gain:";
/// P1, per account. Off: the adapter sends no typing signal.
pub const TYPING: &str = "typing";
/// P2, per Teams account. Off: no read horizon is sent.
pub const RECEIPTS: &str = "receipts";
/// P4. Days messages are kept on this Mac: 7, 30 or 90.
pub const KEEP_DAYS: &str = "keep_days";
/// D2. The image and file cache's cap in MB: 250, 500, 1000 or 2000.
pub const CACHE_MB: &str = "cache_mb";
/// The schema of these rows, for migrations. Not a setting anyone changes.
pub const VERSION: &str = "version";

// Notifications (N1 to N13) and their sounds (S2 to S6), all kept and acted on by chatd.

/// N1. Off: chatd posts no message notification. Counts and the badge keep working.
pub const NOTIFY: &str = "notify";
/// N2 globally (`mentions`, `all`), N3 per account (also `off`), and per conversation
/// (`all`, `mentions`, `off`). Absent in an account or a conversation follows the level above.
pub const NOTIFY_ABOUT: &str = "notify_about";
/// Which kinds of message may notify at all: one-to-one, group chats, mentions in channels,
/// and anything on a Tiny server (its rooms, and a call ringing there).
pub const NOTIFY_DIRECT: &str = "notify_direct";
pub const NOTIFY_GROUPS: &str = "notify_groups";
pub const NOTIFY_MENTIONS: &str = "notify_mentions";
pub const NOTIFY_TINY: &str = "notify_tiny";
/// N4. What a notification shows: `full`, `sender` or `nothing`. The default is `sender`: the
/// sender and "New message", no text. A row is stored only when it differs from the default, so
/// a person who chose `full` under the old default (`full`) has no row and reads `sender` now.
pub const PREVIEW: &str = "preview";
/// N4 while the screen is locked. Windows and Linux; macOS has its own "when unlocked".
pub const PREVIEW_LOCKED: &str = "preview_locked";
/// N5. `ring`, `silent` or `off`.
pub const CALLS: &str = "calls";
/// N6. What message notifications do in a call: `banners`, `sounds` or `nothing`.
pub const IN_CALL: &str = "in_call";
/// N7. Unix seconds until which nothing is posted, or `resume` for until I resume.
pub const PAUSED_UNTIL: &str = "paused_until";
/// N8. Quiet hours, every day, as minutes after local midnight; the window may cross midnight.
pub const QUIET_HOURS: &str = "quiet_hours";
pub const QUIET_FROM: &str = "quiet_from";
pub const QUIET_TO: &str = "quiet_to";
/// N9. A call still rings during a pause or quiet hours.
pub const CALLS_THROUGH: &str = "calls_through";
/// N10. Flash the taskbar button (Windows) or bounce the Dock icon when something notifies
/// while the window is in the background.
pub const FLASH: &str = "flash";
/// N11. The number on the taskbar button or the Dock icon.
pub const BADGE: &str = "badge";
/// S2, S3, S4: the sounds by [`SOUND_CHOICES`] word. S3 may also be `same`.
pub const MESSAGE_SOUND: &str = "message_sound";
pub const MENTION_SOUND: &str = "mention_sound";
pub const RING_SOUND: &str = "ring_sound";
/// S5 per account and S6 per conversation: overrides S2 and S3. Absent follows the level above.
pub const SOUND: &str = "sound";
/// How loud tinyclient plays notification sounds, 0 to 100. Windows and Linux play them
/// itself, so there is no system alert volume to follow there.
pub const ALERT_VOLUME: &str = "alert_volume";

/// The message sounds, by the word stored, then `none`.
pub const SOUND_CHOICES: [&str; 6] = ["pebble", "drop", "tap", "chime", "glass", "none"];
pub const MENTION_SOUND_CHOICES: [&str; 7] = ["same", "pebble", "drop", "tap", "chime", "glass", "none"];
pub const RING_CHOICES: [&str; 3] = ["soft_ring", "classic_ring", "none"];
pub const PREVIEW_CHOICES: [&str; 3] = ["full", "sender", "nothing"];
pub const CALLS_CHOICES: [&str; 3] = ["ring", "silent", "off"];
pub const IN_CALL_CHOICES: [&str; 3] = ["banners", "sounds", "nothing"];
pub const ABOUT_CHOICES: [&str; 2] = ["mentions", "all"];
pub const ACCOUNT_ABOUT_CHOICES: [&str; 3] = ["mentions", "all", "off"];
pub const CONVERSATION_ABOUT_CHOICES: [&str; 3] = ["all", "mentions", "off"];

pub const KEEP_DAYS_CHOICES: [u32; 3] = [7, 30, 90];
pub const CACHE_MB_CHOICES: [u32; 4] = [250, 500, 1000, 2000];
pub const PTT_RELEASE_CHOICES: [u32; 6] = [0, 100, 200, 300, 500, 1000];

/// What a key's values look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Switch(bool),
    Percent(u8),
    Choice(&'static [u32], u32),
    Device,
    Hotkey,
    Gain,
    /// One of these words, and the default.
    Word(&'static [&'static str], &'static str),
    /// Minutes after midnight, 0 to 1439.
    Minute(u32),
    /// Unix seconds, or `resume`.
    Until,
}

fn kind(key: &str) -> Option<Kind> {
    Some(match key {
        KEEP_RUNNING | SOUNDS | CUE_JOINS | CUE_MINE | CUE_CONNECTION => Kind::Switch(true),
        NOTIFY | NOTIFY_DIRECT | NOTIFY_GROUPS | NOTIFY_MENTIONS | NOTIFY_TINY | CALLS_THROUGH | FLASH | BADGE => Kind::Switch(true),
        QUIET_HOURS => Kind::Switch(false),
        QUIET_FROM => Kind::Minute(22 * 60),
        QUIET_TO => Kind::Minute(8 * 60),
        PAUSED_UNTIL => Kind::Until,
        ALERT_VOLUME => Kind::Percent(70),
        PREVIEW => Kind::Word(&PREVIEW_CHOICES, "sender"),
        PREVIEW_LOCKED => Kind::Word(&PREVIEW_CHOICES, "sender"),
        CALLS => Kind::Word(&CALLS_CHOICES, "ring"),
        IN_CALL => Kind::Word(&IN_CALL_CHOICES, "banners"),
        MESSAGE_SOUND => Kind::Word(&SOUND_CHOICES, "pebble"),
        MENTION_SOUND => Kind::Word(&MENTION_SOUND_CHOICES, "same"),
        RING_SOUND => Kind::Word(&RING_CHOICES, "soft_ring"),
        // The words differ by scope (`about_choices`); these inherit, so the default is moot
        // outside the global scope.
        NOTIFY_ABOUT => Kind::Word(&ABOUT_CHOICES, "mentions"),
        SOUND => Kind::Word(&SOUND_CHOICES, "pebble"),
        ECHO_CANCELLATION | NOISE_SUPPRESSION | AUTO_GAIN => Kind::Switch(true),
        JOIN_MUTED | PUSH_TO_TALK | REQUIRE_CALL_E2EE => Kind::Switch(false),
        MUTED_TALK => Kind::Switch(true),
        PTT_RELEASE_MS => Kind::Choice(&PTT_RELEASE_CHOICES, 200),
        CUE_VOLUME => Kind::Percent(50),
        VOICE_VOLUME => Kind::Percent(100),
        SENSITIVITY => Kind::Percent(25),
        INPUT | OUTPUT => Kind::Device,
        PTT_KEY | PRIORITY_KEY | MUTE_KEY | DEAFEN_KEY => Kind::Hotkey,
        _ if gain_user(key).is_some() => Kind::Gain,
        KEEP_DAYS => Kind::Choice(&KEEP_DAYS_CHOICES, 90),
        CACHE_MB => Kind::Choice(&CACHE_MB_CHOICES, 500),
        // Per account; the default is the network's (`typing_default`).
        TYPING | RECEIPTS => Kind::Switch(true),
        LOG_LEVEL => Kind::Word(&LOG_LEVEL_CHOICES, "info"),
        LOG_RING | LOG_EXPORT => Kind::Switch(true),
        _ => return None,
    })
}

/// The scopes a key may be stored in.
fn scoped(key: &str, scope: &str) -> bool {
    match key {
        TYPING => account_of(scope).is_some_and(|account| typing_choice(account.network)),
        RECEIPTS => account_of(scope).is_some_and(|account| account.network == Network::Teams),
        _ if gain_user(key).is_some() => account_of(scope).is_some(),
        NOTIFY_ABOUT => scope == GLOBAL || account_of(scope).is_some() || conversation_of(scope).is_some(),
        SOUND => account_of(scope).is_some() || conversation_of(scope).is_some(),
        _ => scope == GLOBAL,
    }
}

/// Whether `value` may be stored for `key` in `scope`. Anything else is refused, so the table
/// never holds what no build can read.
pub fn valid(scope: &str, key: &str, value: &str) -> bool {
    if !scoped(key, scope) {
        return false;
    }
    match kind(key) {
        Some(Kind::Switch(_)) => matches!(value, "on" | "off"),
        Some(Kind::Percent(_)) => value.parse::<u8>().is_ok_and(|n| n <= 100),
        Some(Kind::Choice(choices, _)) => value.parse::<u32>().is_ok_and(|n| choices.contains(&n)),
        Some(Kind::Device) => !value.is_empty() && value.len() <= 400 && !value.contains(['\n', '\0']),
        Some(Kind::Hotkey) => crate::hotkey::Binding::parse(value).is_some(),
        Some(Kind::Gain) => parse_gain(value).is_some(),
        Some(Kind::Word(_, _)) if key == NOTIFY_ABOUT => about_choices(scope).contains(&value),
        Some(Kind::Word(words, _)) => words.contains(&value),
        Some(Kind::Minute(_)) => value.parse::<u32>().is_ok_and(|n| n < 24 * 60),
        Some(Kind::Until) => value == "resume" || value.parse::<u64>().is_ok(),
        None => false,
    }
}

/// What `notify_about` may be in a scope: the global one has no `off`, which is N1's.
pub fn about_choices(scope: &str) -> &'static [&'static str] {
    if scope == GLOBAL {
        &ABOUT_CHOICES
    } else if account_of(scope).is_some() {
        &ACCOUNT_ABOUT_CHOICES
    } else {
        &CONVERSATION_ABOUT_CHOICES
    }
}

/// The setting that keeps one person's volume for me (V9), in their account's scope.
pub fn gain_key(user_id: &str) -> String {
    format!("{GAIN_PREFIX}{user_id}")
}

/// The user a `gain:` key names. Ids are short and have no spaces.
fn gain_user(key: &str) -> Option<&str> {
    key.strip_prefix(GAIN_PREFIX).filter(|id| !id.is_empty() && id.len() <= 128 && !id.contains(char::is_whitespace))
}

/// A stored gain: `150`, or `0 muted`. Percent above [`crate::GAIN_MAX`] is refused.
fn parse_gain(value: &str) -> Option<(u16, bool)> {
    let (percent, muted) = match value.strip_suffix(" muted") {
        Some(percent) => (percent, true),
        None => (value, false),
    };
    percent.parse::<u16>().ok().filter(|p| *p <= crate::GAIN_MAX).map(|p| (p, muted))
}

/// How a gain is stored: `None` for the default (100 % and heard), which is not.
pub fn gain_value(percent: u16, muted: bool) -> Option<String> {
    let percent = percent.min(crate::GAIN_MAX);
    match (percent, muted) {
        (100, false) => None,
        (_, false) => Some(percent.to_string()),
        (_, true) => Some(format!("{percent} muted")),
    }
}

/// Whether the key may be reset in this scope (a `Set` with no value).
pub fn known(scope: &str, key: &str) -> bool {
    kind(key).is_some() && scoped(key, scope)
}

fn network_word(network: Network) -> &'static str {
    match network {
        Network::Telegram => "telegram",
        Network::Discord => "discord",
        Network::Slack => "slack",
        Network::Teams => "teams",
        Network::Signal => "signal",
        Network::Messages => "messages",
        Network::Tiny => "tiny",
    }
}

pub fn account_scope(account: &AccountKey) -> String {
    format!("account:{}:{}", network_word(account.network), account.account)
}

pub fn conversation_scope(conversation: &Key) -> String {
    format!("conv:{}:{}:{}", network_word(conversation.network), conversation.account, conversation.id)
}

/// The network of a conversation's scope, `conv:<network>:<account>:<id>` with every part.
pub fn conversation_of(scope: &str) -> Option<Network> {
    let (word, rest) = scope.strip_prefix("conv:")?.split_once(':')?;
    let network = Network::ALL.into_iter().find(|n| network_word(*n) == word)?;
    let (account, id) = rest.split_once(':')?;
    (!account.is_empty() && !id.is_empty()).then_some(network)
}

/// The account an `account:` scope names.
pub fn account_of(scope: &str) -> Option<AccountKey> {
    let (word, account) = scope.strip_prefix("account:")?.split_once(':')?;
    let network = Network::ALL.into_iter().find(|n| network_word(*n) == word)?;
    (!account.is_empty()).then(|| AccountKey { network, account: account.to_owned() })
}

/// Whether the user decides if this network is told I am typing. Telegram always sends it and
/// Signal follows the phone's choice, so neither has a switch (P1).
pub fn typing_choice(network: Network) -> bool {
    matches!(network, Network::Discord | Network::Slack | Network::Teams)
}

/// P1's default: Discord off (its typing is loud in busy channels), the rest on.
pub fn typing_default(network: Network) -> bool {
    network != Network::Discord
}

/// Which microphone or speaker the user chose (V1, V2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceChoice {
    /// Never chosen: the system default when that is hardware, else a headset or speakers. A
    /// virtual device (Krisp, VB-Cable, ...) is never picked this way.
    #[default]
    Auto,
    /// "System default": wherever the system default goes, virtual or not, live.
    System,
    /// One device. The id is the system's (a WASAPI endpoint id, a CoreAudio UID) and is tried
    /// first; the name finds it when the id is unknown or no longer listed.
    Device { name: String, id: Option<String> },
}

impl DeviceChoice {
    /// A stored value, or `MediaRequest::Devices`' field. `None` is [`DeviceChoice::Auto`].
    pub fn parse(value: Option<&str>) -> DeviceChoice {
        match value {
            None | Some("") => DeviceChoice::Auto,
            Some(SYSTEM_DEFAULT) => DeviceChoice::System,
            Some(value) => match value.split_once('\t') {
                Some((name, id)) if !id.is_empty() => DeviceChoice::Device { name: name.to_owned(), id: Some(id.to_owned()) },
                Some((name, _)) => DeviceChoice::Device { name: name.to_owned(), id: None },
                None => DeviceChoice::Device { name: value.to_owned(), id: None },
            },
        }
    }

    /// What is stored for it. `None` for [`DeviceChoice::Auto`].
    pub fn value(&self) -> Option<String> {
        match self {
            DeviceChoice::Auto => None,
            DeviceChoice::System => Some(SYSTEM_DEFAULT.to_owned()),
            DeviceChoice::Device { name, id: None } => Some(name.clone()),
            DeviceChoice::Device { name, id: Some(id) } => Some(format!("{name}\t{id}")),
        }
    }

    /// One device, by its name and id as listed.
    pub fn device(name: &str, id: &str) -> DeviceChoice {
        let id = (!id.is_empty()).then(|| id.replace(['\t', '\n', '\0'], ""));
        DeviceChoice::Device { name: name.replace(['\t', '\n', '\0'], " "), id }
    }

    /// True when `device` is the one chosen: by id when the choice has one that is listed, else
    /// by name (the first of that name, as a call would open).
    pub fn is(&self, device: &crate::AudioDevice, listed: &[crate::AudioDevice]) -> bool {
        let DeviceChoice::Device { name, id } = self else {
            return false;
        };
        match id.as_ref().filter(|id| listed.iter().any(|d| &d.id == *id)) {
            Some(id) => &device.id == id,
            None => listed.iter().find(|d| &d.name == name).is_some_and(|d| d.id == device.id),
        }
    }

    /// The choice with the id and name a listed device has now, when it lacks or differs from
    /// them: a name stored by an older build gains its id, a renamed device its new name.
    /// `None` when nothing needs storing.
    pub fn upgrade(&self, listed: &[crate::AudioDevice]) -> Option<DeviceChoice> {
        let DeviceChoice::Device { name, id } = self else {
            return None;
        };
        let found = match id {
            Some(id) => listed.iter().find(|d| &d.id == id),
            None => listed.iter().find(|d| &d.name == name && !d.id.is_empty()),
        }?;
        let better = DeviceChoice::device(&found.name, &found.id);
        (better != *self).then_some(better)
    }

    /// The device's name, for words. `None` for the two that follow the system.
    pub fn name(&self) -> Option<&str> {
        match self {
            DeviceChoice::Device { name, .. } => Some(name),
            _ => None,
        }
    }
}

/// Every stored value, read with defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values(BTreeMap<(String, String), String>);

impl Values {
    pub fn new(settings: impl IntoIterator<Item = Setting>) -> Values {
        Values(settings.into_iter().map(|s| ((s.scope, s.key), s.value)).collect())
    }

    /// The stored value, if one is.
    pub fn stored(&self, scope: &str, key: &str) -> Option<&str> {
        self.0.get(&(scope.to_owned(), key.to_owned())).map(String::as_str).filter(|value| valid(scope, key, value))
    }

    pub fn set(&mut self, scope: &str, key: &str, value: Option<String>) {
        let at = (scope.to_owned(), key.to_owned());
        match value {
            Some(value) => self.0.insert(at, value),
            None => self.0.remove(&at),
        };
    }

    /// A global switch.
    pub fn on(&self, key: &str) -> bool {
        let default = matches!(kind(key), Some(Kind::Switch(true)));
        self.stored(GLOBAL, key).map_or(default, |value| value == "on")
    }

    /// A global percent or choice.
    pub fn number(&self, key: &str) -> u32 {
        let default = match kind(key) {
            Some(Kind::Percent(n)) => u32::from(n),
            Some(Kind::Choice(_, n)) => n,
            _ => 0,
        };
        self.stored(GLOBAL, key).and_then(|value| value.parse().ok()).unwrap_or(default)
    }

    /// The default of a number, for the reset button.
    pub fn default_number(key: &str) -> u32 {
        Values::default().number(key)
    }

    /// The stored value of V1 or V2, as `MediaRequest::Devices` carries it.
    pub fn device(&self, key: &str) -> Option<String> {
        self.stored(GLOBAL, key).map(str::to_owned)
    }

    /// V1 or V2, read.
    pub fn device_choice(&self, key: &str) -> DeviceChoice {
        DeviceChoice::parse(self.stored(GLOBAL, key))
    }

    /// P1 for one account.
    pub fn typing(&self, account: &AccountKey) -> bool {
        if !typing_choice(account.network) {
            return true;
        }
        let stored = self.stored(&account_scope(account), TYPING);
        stored.map_or(typing_default(account.network), |value| value == "on")
    }

    /// P2 for one account. Only Teams has a switch; elsewhere it is the network's to decide.
    pub fn receipts(&self, account: &AccountKey) -> bool {
        self.stored(&account_scope(account), RECEIPTS).is_none_or(|value| value == "on")
    }

    /// The in-call sound and gain choices, as mediad takes them. S1 off silences every cue.
    pub fn tuning(&self) -> crate::Tuning {
        let sounds = self.on(SOUNDS);
        crate::Tuning {
            voice: self.number(VOICE_VOLUME).min(100) as u8,
            gate: self.number(SENSITIVITY).min(100) as u8,
            cues: self.number(CUE_VOLUME).min(100) as u8,
            joins: sounds && self.on(CUE_JOINS),
            mine: sounds && self.on(CUE_MINE),
            connection: sounds && self.on(CUE_CONNECTION),
        }
    }

    /// How the microphone opens, as the user chose. A room that requires push-to-talk asks
    /// for [`Values::push_to_talk`] instead.
    pub fn input_mode(&self) -> crate::InputMode {
        if self.on(PUSH_TO_TALK) {
            self.push_to_talk()
        } else {
            crate::InputMode::Voice { threshold: self.number(SENSITIVITY).min(100) as u8 }
        }
    }

    /// Push-to-talk with the user's release delay, whatever the mode chosen.
    pub fn push_to_talk(&self) -> crate::InputMode {
        crate::InputMode::PushToTalk { release_ms: self.number(PTT_RELEASE_MS).min(1000) as u16 }
    }

    pub fn processing(&self) -> crate::AudioProcessing {
        crate::AudioProcessing {
            echo_cancellation: self.on(ECHO_CANCELLATION),
            noise_suppression: self.on(NOISE_SUPPRESSION),
            auto_gain: self.on(AUTO_GAIN),
        }
    }

    /// A global key binding (V8, V13 to V15), if one is set.
    pub fn hotkey(&self, key: &str) -> Option<crate::hotkey::Binding> {
        self.stored(GLOBAL, key).and_then(crate::hotkey::Binding::parse)
    }

    /// Everyone on `account` whose volume for me is not the default: user id, percent, muted.
    pub fn gains(&self, account: &AccountKey) -> Vec<(String, u16, bool)> {
        let scope = account_scope(account);
        self.0
            .iter()
            .filter(|((at, _), _)| *at == scope)
            .filter_map(|((_, key), value)| {
                let (percent, muted) = parse_gain(value)?;
                Some((gain_user(key)?.to_owned(), percent, muted))
            })
            .collect()
    }

    /// A word-valued setting in a scope, or its default when nothing valid is stored there.
    /// A scope that inherits asks [`Values::stored`] instead.
    pub fn word(&self, scope: &str, key: &str) -> &str {
        let default = match kind(key) {
            Some(Kind::Word(_, default)) => default,
            _ => "",
        };
        self.stored(scope, key).unwrap_or(default)
    }

    /// A time of day (N8), in minutes after midnight.
    pub fn minute(&self, key: &str) -> u32 {
        let default = match kind(key) {
            Some(Kind::Minute(n)) => n,
            _ => 0,
        };
        self.stored(GLOBAL, key).and_then(|value| value.parse().ok()).unwrap_or(default)
    }

    /// N7 at `now`: `None` when not paused, `Some(None)` until I resume, else until when.
    pub fn paused(&self, now: i64) -> Option<Option<i64>> {
        match self.stored(GLOBAL, PAUSED_UNTIL)? {
            "resume" => Some(None),
            until => until.parse::<i64>().ok().filter(|until| *until > now).map(Some),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Setting> + '_ {
        self.0.iter().map(|((scope, key), value)| Setting { scope: scope.clone(), key: key.clone(), value: value.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(network: Network) -> AccountKey {
        AccountKey { network, account: "T1".into() }
    }

    #[test]
    fn missing_rows_read_as_defaults_and_junk_reads_as_missing() {
        let values = Values::default();
        assert!(values.on(KEEP_RUNNING) && values.on(SOUNDS) && !values.on(JOIN_MUTED));
        assert_eq!((values.number(KEEP_DAYS), values.number(CACHE_MB), values.number(SENSITIVITY)), (90, 500, 25));
        assert_eq!((values.number(CUE_VOLUME), values.number(VOICE_VOLUME)), (50, 100));
        let junk = Values::new([
            Setting { scope: GLOBAL.into(), key: KEEP_DAYS.into(), value: "12".into() },
            Setting { scope: GLOBAL.into(), key: SOUNDS.into(), value: "yes".into() },
            Setting { scope: GLOBAL.into(), key: "proxy".into(), value: "socks5://x".into() },
        ]);
        assert_eq!(junk.number(KEEP_DAYS), 90);
        assert!(junk.on(SOUNDS));
    }

    #[test]
    fn only_known_keys_in_their_own_scopes_take_values_that_parse() {
        assert!(valid(GLOBAL, KEEP_DAYS, "7") && !valid(GLOBAL, KEEP_DAYS, "8"));
        assert!(valid(GLOBAL, CUE_VOLUME, "0") && valid(GLOBAL, CUE_VOLUME, "100") && !valid(GLOBAL, CUE_VOLUME, "101"));
        assert!(!valid(GLOBAL, "proxy", "on"));
        assert!(!valid(GLOBAL, TYPING, "off"), "typing is per account");
        let discord = account_scope(&account(Network::Discord));
        assert!(valid(&discord, TYPING, "on"));
        assert!(!valid(&account_scope(&account(Network::Telegram)), TYPING, "off"), "Telegram always sends it");
        assert!(!valid(&discord, RECEIPTS, "off"), "Discord has no receipts");
        assert!(valid(&account_scope(&account(Network::Teams)), RECEIPTS, "off"));
        assert!(valid(GLOBAL, INPUT, "Studio Display Microphone") && !valid(GLOBAL, INPUT, ""));
    }

    #[test]
    fn requiring_end_to_end_encrypted_calls_is_a_switch_that_is_off_until_chosen() {
        // Off by default on purpose: a server or an app that cannot encrypt would stop working.
        assert!(!Values::default().on(REQUIRE_CALL_E2EE));
        assert!(valid(GLOBAL, REQUIRE_CALL_E2EE, "on") && valid(GLOBAL, REQUIRE_CALL_E2EE, "off"));
        assert!(!valid(GLOBAL, REQUIRE_CALL_E2EE, "yes") && !valid(GLOBAL, REQUIRE_CALL_E2EE, "1") && !valid(GLOBAL, REQUIRE_CALL_E2EE, ""));
        assert!(!valid(&account_scope(&account(Network::Tiny)), REQUIRE_CALL_E2EE, "on"), "it is global");
        assert!(known(GLOBAL, REQUIRE_CALL_E2EE));
        let mut values = Values::default();
        values.set(GLOBAL, REQUIRE_CALL_E2EE, Some("on".into()));
        assert!(values.on(REQUIRE_CALL_E2EE));
        // A stored value no build can read falls back to the default, which is off.
        values.set(GLOBAL, REQUIRE_CALL_E2EE, Some("maybe".into()));
        assert!(!values.on(REQUIRE_CALL_E2EE));
    }

    #[test]
    fn typing_defaults_follow_the_network_and_the_switch_overrides_it() {
        let mut values = Values::default();
        let (discord, slack, telegram) = (account(Network::Discord), account(Network::Slack), account(Network::Telegram));
        assert!(!values.typing(&discord) && values.typing(&slack) && values.typing(&telegram));
        values.set(&account_scope(&discord), TYPING, Some("on".into()));
        values.set(&account_scope(&slack), TYPING, Some("off".into()));
        assert!(values.typing(&discord) && !values.typing(&slack));
        assert_eq!(account_of(&account_scope(&slack)), Some(slack));
        assert_eq!(account_of("account:myspace:1"), None);
    }

    #[test]
    fn the_microphone_opens_by_voice_until_push_to_talk_is_chosen_and_processing_is_on() {
        let mut values = Values::default();
        assert_eq!(values.input_mode(), crate::InputMode::Voice { threshold: 25 });
        assert_eq!(values.processing(), crate::AudioProcessing::default());
        assert!(valid(GLOBAL, PTT_RELEASE_MS, "300") && !valid(GLOBAL, PTT_RELEASE_MS, "250"));
        values.set(GLOBAL, PUSH_TO_TALK, Some("on".into()));
        assert_eq!(values.input_mode(), crate::InputMode::PushToTalk { release_ms: 200 });
        values.set(GLOBAL, PTT_RELEASE_MS, Some("0".into()));
        values.set(GLOBAL, NOISE_SUPPRESSION, Some("off".into()));
        assert_eq!(values.input_mode(), crate::InputMode::PushToTalk { release_ms: 0 });
        assert!(!values.processing().noise_suppression && values.processing().echo_cancellation);
    }

    #[test]
    fn keys_are_bindings_and_a_gain_is_kept_per_account_and_person() {
        assert!(valid(GLOBAL, PTT_KEY, "mouse4") && valid(GLOBAL, MUTE_KEY, "ctrl+shift+m"));
        assert!(!valid(GLOBAL, PTT_KEY, "hyper+q") && !valid(GLOBAL, PTT_KEY, ""));
        let tiny = account_scope(&account(Network::Tiny));
        let key = gain_key("u42");
        assert!(valid(&tiny, &key, "150") && valid(&tiny, &key, "0 muted") && valid(&tiny, &key, "200"));
        assert!(!valid(&tiny, &key, "201") && !valid(&tiny, &key, "loud") && !valid(GLOBAL, &key, "50"));
        assert!(!valid(&tiny, "gain:", "50") && !valid(&tiny, "gain:a b", "50"));
        assert!(known(&tiny, &key));
        assert_eq!((gain_value(100, false), gain_value(250, false), gain_value(30, true)), (None, Some("200".into()), Some("30 muted".into())));
        let mut values = Values::default();
        values.set(&tiny, &key, Some("30 muted".into()));
        values.set(&tiny, &gain_key("u7"), Some("180".into()));
        values.set(&account_scope(&account(Network::Discord)), &gain_key("u9"), Some("50".into()));
        let mut gains = values.gains(&account(Network::Tiny));
        gains.sort();
        assert_eq!(gains, [("u42".to_owned(), 30, true), ("u7".to_owned(), 180, false)]);
        values.set(GLOBAL, PTT_KEY, Some("f13".into()));
        assert_eq!(values.hotkey(PTT_KEY).map(|b| b.to_string()), Some("f13".into()));
        assert_eq!(values.hotkey(DEAFEN_KEY), None);
    }

    #[test]
    fn notification_settings_default_and_take_their_words_by_scope() {
        let values = Values::default();
        assert!(values.on(NOTIFY) && values.on(NOTIFY_DIRECT) && values.on(CALLS_THROUGH) && !values.on(QUIET_HOURS));
        assert_eq!((values.word(GLOBAL, NOTIFY_ABOUT), values.word(GLOBAL, PREVIEW), values.word(GLOBAL, PREVIEW_LOCKED)), ("mentions", "sender", "sender"));
        assert_eq!((values.word(GLOBAL, MESSAGE_SOUND), values.word(GLOBAL, MENTION_SOUND), values.word(GLOBAL, RING_SOUND)), ("pebble", "same", "soft_ring"));
        assert_eq!((values.minute(QUIET_FROM), values.minute(QUIET_TO), values.number(ALERT_VOLUME)), (1320, 480, 70));
        let discord = account_scope(&account(Network::Discord));
        let channel = conversation_scope(&Key { network: Network::Discord, account: "T1".into(), id: "C9".into() });
        // `off` is a level of an account and a conversation, never the global one (that is N1).
        assert!(!valid(GLOBAL, NOTIFY_ABOUT, "off") && valid(&discord, NOTIFY_ABOUT, "off") && valid(&channel, NOTIFY_ABOUT, "off"));
        assert!(valid(&channel, NOTIFY_ABOUT, "all") && !valid(&channel, NOTIFY_ABOUT, "loud"));
        assert!(valid(&channel, SOUND, "glass") && valid(&discord, SOUND, "none") && !valid(GLOBAL, SOUND, "glass"));
        assert!(!valid(&channel, SOUND, "classic_ring") && valid(GLOBAL, RING_SOUND, "classic_ring"));
        assert!(!valid("conv:discord:T1", SOUND, "tap") && !valid("conv:myspace:T1:C", SOUND, "tap"));
        assert!(valid(GLOBAL, QUIET_FROM, "0") && valid(GLOBAL, QUIET_FROM, "1439") && !valid(GLOBAL, QUIET_FROM, "1440"));
        assert!(!valid(&discord, PREVIEW, "full"), "preview is global");
        assert!(known(&channel, SOUND) && !known(GLOBAL, SOUND));
        assert_eq!(values.stored(&channel, NOTIFY_ABOUT), None);
    }

    #[test]
    fn a_stored_preview_is_never_rewritten_by_a_new_default() {
        // The default moved from `full` to `sender`: what a person stored stays what it was.
        let stored = |word: &str| Values::new([Setting { scope: GLOBAL.into(), key: PREVIEW.into(), value: word.into() }]);
        assert_eq!(stored("full").word(GLOBAL, PREVIEW), "full");
        assert_eq!(stored("nothing").word(GLOBAL, PREVIEW), "nothing");
        assert_eq!(stored("sender").word(GLOBAL, PREVIEW), "sender");
        // Nothing stored is the default, and a word no build knows is nothing stored.
        assert_eq!(Values::default().word(GLOBAL, PREVIEW), "sender");
        assert_eq!(stored("everything").word(GLOBAL, PREVIEW), "sender");
        // The lock screen has its own word and its own default, which did not move.
        assert_eq!(stored("full").word(GLOBAL, PREVIEW_LOCKED), "sender");
    }

    #[test]
    fn a_pause_lasts_until_its_time_or_until_resumed() {
        let mut values = Values::default();
        assert_eq!(values.paused(1_000), None);
        values.set(GLOBAL, PAUSED_UNTIL, Some("2000".into()));
        assert_eq!((values.paused(1_000), values.paused(2_000)), (Some(Some(2_000)), None));
        values.set(GLOBAL, PAUSED_UNTIL, Some("resume".into()));
        assert_eq!(values.paused(i64::MAX), Some(None));
        assert!(!valid(GLOBAL, PAUSED_UNTIL, "soon"));
    }

    #[test]
    fn play_sounds_off_silences_every_cue_and_keeps_the_choices() {
        let mut values = Values::default();
        values.set(GLOBAL, CUE_MINE, Some("off".into()));
        let tuning = values.tuning();
        assert!(tuning.joins && !tuning.mine && tuning.connection);
        values.set(GLOBAL, SOUNDS, Some("off".into()));
        let tuning = values.tuning();
        assert!(!tuning.joins && !tuning.mine && !tuning.connection);
        assert_eq!((tuning.voice, tuning.gate, tuning.cues), (100, 25, 50));
        assert_eq!(values.stored(GLOBAL, CUE_MINE), Some("off"));
    }

    fn listed(id: &str, name: &str) -> crate::AudioDevice {
        crate::AudioDevice { id: id.into(), name: name.into(), ..Default::default() }
    }

    #[test]
    fn a_device_choice_round_trips_and_a_name_stored_before_ids_still_reads() {
        const HEADSET: &str = "Headset Microphone (CORSAIR HS55 Wireless Core Receiver)";
        let id = "wasapi:{0.0.1.00000000}.{7f1c0d2e-0000-4000-8000-0123456789ab}";
        for choice in [DeviceChoice::Auto, DeviceChoice::System, DeviceChoice::device(HEADSET, id), DeviceChoice::device(HEADSET, "")] {
            let value = choice.value();
            assert!(value.as_deref().is_none_or(|value| valid(GLOBAL, INPUT, value)), "{value:?}");
            assert_eq!(DeviceChoice::parse(value.as_deref()), choice);
        }
        let mut values = Values::default();
        assert_eq!(values.device_choice(INPUT), DeviceChoice::Auto, "never chosen");
        values.set(GLOBAL, INPUT, Some(HEADSET.into()));
        assert_eq!(values.device_choice(INPUT), DeviceChoice::Device { name: HEADSET.into(), id: None });
        values.set(GLOBAL, OUTPUT, Some(SYSTEM_DEFAULT.into()));
        assert_eq!(values.device_choice(OUTPUT), DeviceChoice::System);
    }

    #[test]
    fn a_name_alone_gains_its_id_once_listed_and_an_id_follows_a_rename() {
        let legacy = DeviceChoice::Device { name: "Mic".into(), id: None };
        assert_eq!(legacy.upgrade(&[]), None, "not listed: nothing to learn");
        let list = [listed("a", "Other"), listed("b", "Mic")];
        assert_eq!(legacy.upgrade(&list), Some(DeviceChoice::device("Mic", "b")));
        let current = DeviceChoice::device("Mic", "b");
        assert_eq!(current.upgrade(&list), None, "already current");
        let renamed = [listed("b", "Mic (2- USB)")];
        assert_eq!(current.upgrade(&renamed), Some(DeviceChoice::device("Mic (2- USB)", "b")));
        assert_eq!(DeviceChoice::System.upgrade(&list), None);
    }

    #[test]
    fn the_chosen_row_is_found_by_id_and_by_name_when_the_id_is_not_listed() {
        let list = [listed("a", "Mic"), listed("b", "Mic")];
        assert!(DeviceChoice::device("Mic", "b").is(&list[1], &list));
        assert!(!DeviceChoice::device("Mic", "b").is(&list[0], &list), "two of one model: the id tells");
        assert!(DeviceChoice::device("Mic", "gone").is(&list[0], &list), "the first of that name");
        assert!(!DeviceChoice::Auto.is(&list[0], &list));
    }
}
