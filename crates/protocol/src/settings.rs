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

/// Register as a handler for networks' own URI schemes (`discord://`, `slack://`, `tg://`, `msteams://`).
/// Off by default so tinyclient never silently takes over links from an official app.
pub const HANDLE_NETWORK_SCHEMES: &str = "handle_network_schemes";
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
/// V22. The camera, stored as V1 and V2 store theirs: absent is the first camera the system
/// lists, else one by name, then a tab and the system's id for it.
pub const CAMERA: &str = "camera";
/// V23. Camera resolution/rate tier: `auto`, `hd`, `standard`, or `data_saver`.
pub const CAMERA_QUALITY: &str = "camera_quality";
pub const CAMERA_QUALITY_CHOICES: [&str; 4] = ["auto", "hd", "standard", "data_saver"];
/// V24. Mirror the local camera self-view horizontally (never what peers see). Defaults on.
pub const CAMERA_MIRROR: &str = "camera_mirror";
/// V25. Apply lightweight on-device background blur before encoding camera video. Defaults off.
pub const CAMERA_BLUR: &str = "camera_blur";
/// The stored value of "System default" in V1 and V2.
pub const SYSTEM_DEFAULT: &str = "@system";
/// V4. Every join and every Accept starts muted.
pub const JOIN_MUTED: &str = "join_muted";
/// V19: "You're muted" shows when the microphone hears speech while I am muted in a call.
pub const MUTED_TALK: &str = "muted_talk";
/// V27: automatically open a newly started screen share when the user is looking at the call.
pub const AUTO_WATCH_SHARE: &str = "auto_watch_share";
/// Only join Tiny calls that are end-to-end encrypted. A call whose server turns encryption off
/// always stops sending and playing (mediad `Blocked`); off, the person may choose to carry on
/// unencrypted, on, they may not, and a server that cannot encrypt is not joined. Off by default:
/// an older server or an older app in the room cannot do it. An Incognito host
/// ([`crate::incognito_host`]) or an `e2ee_required` room acts as on whatever this says. See
/// `SECURITY.md`.
pub const REQUIRE_CALL_E2EE: &str = "require_call_e2ee";
/// PS1. Streamer mode: `off`, `on`, or `while_sharing` (default).
pub const STREAMER_MODE: &str = "streamer_mode";
pub const STREAMER_MODE_CHOICES: [&str; 3] = ["off", "on", "while_sharing"];
/// PS2. Exclude tinyclient's own window from capture by other applications while Streamer mode is active.
pub const STREAMER_HIDE_WINDOW: &str = "streamer_hide_window";
/// PS3. Automatically activate Streamer mode while OBS Studio or Streamlabs Desktop is running.
pub const STREAMER_DETECT_OBS: &str = "streamer_detect_obs";
/// V5. Everyone's voice, 0 to 100.
pub const VOICE_VOLUME: &str = "voice_volume";
/// V28. Discord soundboard sounds, 0 to 100. 0 plays nothing.
pub const SOUNDBOARD_VOLUME: &str = "soundboard_volume";
/// Sound ids the soundboard picker keeps at the front, comma or newline separated.
pub const SOUNDBOARD_FAVORITES: &str = "soundboard_favorites";
/// V6. The level, 0 to 100, from which our microphone counts as speech, and in voice
/// activation opens.
pub const SENSITIVITY: &str = "sensitivity";
/// V8. On: the microphone opens only while the push-to-talk key is held.
pub const PUSH_TO_TALK: &str = "push_to_talk";
/// V8. How long the microphone stays open after push-to-talk is let go, in milliseconds.
pub const PTT_RELEASE_MS: &str = "ptt_release_ms";
/// V10, V11, V12. The microphone's processing. V11 is [`NOISE_CHOICES`]: off, standard, high, enhanced.
pub const ECHO_CANCELLATION: &str = "echo_cancellation";
pub const NOISE_SUPPRESSION: &str = "noise_suppression";
pub const AUTO_GAIN: &str = "auto_gain";
/// Mic soft gain before processing, 0 to 100. 100 is as captured.
pub const INPUT_VOLUME: &str = "input_volume";
/// Opus encode quality within a Tiny room's bitrate: [`QUALITY_CHOICES`].
pub const VOICE_QUALITY: &str = "voice_quality";
pub const NOISE_CHOICES: [&str; 4] = ["off", "standard", "high", "enhanced"];
pub const QUALITY_CHOICES: [&str; 3] = ["data_saver", "standard", "high"];
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
/// Local switch: always relay Telegram calls through Telegram servers, never offering a direct
/// peer-to-peer path regardless of the account's peer-to-peer setting.
pub const TELEGRAM_ALWAYS_RELAY: &str = "telegram_always_relay";
/// Local gates beside Telegram's auto-download presets. Off until the account turns them on.
pub const TELEGRAM_DOWNLOAD_PHOTOS: &str = "telegram_download_photos";
pub const TELEGRAM_DOWNLOAD_PRIVATE: &str = "telegram_download_private";
pub const TELEGRAM_DOWNLOAD_GROUPS: &str = "telegram_download_groups";
pub const TELEGRAM_DOWNLOAD_CHANNELS: &str = "telegram_download_channels";

/// Whether a Telegram call's media must run relay-only (no host or reflexive ICE candidates).
/// Obeys both the per-call server promise (`p2p_allowed == false`) and the local always-relay
/// switch (`always_relay == true`).
pub fn telegram_relay_only(p2p_allowed: bool, always_relay: bool) -> bool {
    !p2p_allowed || always_relay
}

/// P1, per account. Off: the adapter sends no typing signal.
pub const TYPING: &str = "typing";
/// P2, per Teams or Signal account. Off: nothing is sent that others would read as a receipt.
pub const RECEIPTS: &str = "receipts";
/// Per Teams account. On (default): sends `user.activity` active heartbeats while the window is
/// focused and receiving input so colleagues do not see you as Away. Turn off to avoid suppressing
/// Teams mobile notifications.
pub const PRESENCE_ACTIVITY: &str = "presence_activity";
/// P4. Days messages are kept on this Mac: 7, 30 or 90.
pub const KEEP_DAYS: &str = "keep_days";
/// M8. Target language for inline message translation (`"auto"` follows system locale).
pub const TRANSLATE_TO: &str = "translate_to";
pub const TRANSLATE_TO_CHOICES: [&str; 13] = [
    "auto", "en", "es", "fr", "de", "it", "pt", "ru", "ja", "ko", "zh", "ar", "hi",
];
/// Client-side link preview cards (e.g. Google Messages). On for known contacts by default.
pub const LINK_PREVIEWS: &str = "link_previews";
/// Client-side link preview cards for numbers not in contacts. Off by default to avoid
/// fetching phishing links automatically.
pub const LINK_PREVIEWS_UNKNOWN: &str = "link_previews_unknown";
/// D2. The image and file cache's cap in MB: 250, 500, 1000 or 2000.
pub const CACHE_MB: &str = "cache_mb";
/// Preferred target language for on-demand message translation (`auto` or a short BCP-47 tag like `en`, `es`, `zh-Hans`).
pub const TRANSLATE_LANG: &str = "translate_lang";
/// The schema of these rows, for migrations. Not a setting anyone changes.
pub const VERSION: &str = "version";

// Network proxy (Settings › General › Network).
/// `system` (default), `none`, or `custom`.
pub const PROXY_MODE: &str = "proxy_mode";
pub const PROXY_MODE_CHOICES: [&str; 3] = ["system", "none", "custom"];
/// `http` (HTTP CONNECT) or `socks5`.
pub const PROXY_SCHEME: &str = "proxy_scheme";
pub const PROXY_SCHEME_CHOICES: [&str; 2] = ["http", "socks5"];
/// Proxy hostname or IP address (no scheme or port).
pub const PROXY_HOST: &str = "proxy_host";
/// Proxy TCP port, 1..=65535.
pub const PROXY_PORT: &str = "proxy_port";
/// Optional proxy username; password is stored in the system keychain, never in SQLite.
pub const PROXY_USER: &str = "proxy_user";

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
/// Reactions to my messages: `off`, `direct` (in direct messages), or `everywhere`.
pub const NOTIFY_REACTIONS: &str = "notify_reactions";
pub const REACTION_NOTIFY_CHOICES: [&str; 3] = ["off", "direct", "everywhere"];
/// Replies in threads I follow (`Filing::followed`).
pub const NOTIFY_THREADS: &str = "notify_threads";
/// Comma- or newline-separated keywords that trigger a notification on word boundaries.
pub const NOTIFY_KEYWORDS: &str = "notify_keywords";
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
/// N10. Ask for attention when something notifies while the window is in the background: the
/// taskbar button flashes (Windows), the Dock icon bounces (macOS), the window manager's
/// urgency hint is set (Linux).
pub const FLASH: &str = "flash";
/// N11. The number on the taskbar button (Windows), the Dock icon (macOS) or the launcher
/// icon (Linux).
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
/// S14. A ring back while I am calling someone, until they answer or I cancel.
pub const RINGBACK: &str = "ringback";
/// S13. One row per file the user added, in a `sound:<id>` scope. The value is the name shown
/// in the pickers; the file is `<id>.wav` in the sounds directory (`sound_path`).
pub const CUSTOM_SOUND: &str = "custom_sound";
/// The scope a [`CUSTOM_SOUND`] row lives in.
pub const SOUND_SCOPE_PREFIX: &str = "sound:";

/// The message sounds, by the word stored, then `none`. A custom sound is `custom:<id>`.
pub const SOUND_CHOICES: [&str; 8] = ["pebble", "drop", "tap", "chime", "glass", "marimba", "bell", "none"];
pub const MENTION_SOUND_CHOICES: [&str; 9] = ["same", "pebble", "drop", "tap", "chime", "glass", "marimba", "bell", "none"];
pub const RING_CHOICES: [&str; 4] = ["soft_ring", "classic_ring", "ripple", "none"];
/// The most custom sounds a picker holds, so the menu stays one screen.
pub const CUSTOM_SOUND_LIMIT: usize = 12;
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
    /// A custom sound's name, in its `sound:<id>` scope.
    CustomSound,
    /// A short list of comma/newline-separated notification keywords.
    Keywords,
    /// A hostname or IP address for a custom proxy.
    Host,
    /// A TCP port number, 1..=65535.
    Port,
    /// An optional proxy username.
    User,
    /// Preferred translation language (`auto` or a short BCP-47 tag).
    Language,
}

fn kind(key: &str) -> Option<Kind> {
    Some(match key {
        KEEP_RUNNING | SOUNDS | CUE_JOINS | CUE_MINE | CUE_CONNECTION | LINK_PREVIEWS => Kind::Switch(true),
        NOTIFY | NOTIFY_DIRECT | NOTIFY_GROUPS | NOTIFY_MENTIONS | NOTIFY_TINY | NOTIFY_THREADS | CALLS_THROUGH | FLASH | BADGE | RINGBACK => Kind::Switch(true),
        NOTIFY_REACTIONS => Kind::Word(&REACTION_NOTIFY_CHOICES, "off"),
        NOTIFY_KEYWORDS => Kind::Keywords,
        PROXY_MODE => Kind::Word(&PROXY_MODE_CHOICES, "system"),
        PROXY_SCHEME => Kind::Word(&PROXY_SCHEME_CHOICES, "http"),
        PROXY_HOST => Kind::Host,
        PROXY_PORT => Kind::Port,
        PROXY_USER => Kind::User,
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
        ECHO_CANCELLATION | AUTO_GAIN => Kind::Switch(true),
        NOISE_SUPPRESSION => Kind::Word(&NOISE_CHOICES, "enhanced"),
        VOICE_QUALITY => Kind::Word(&QUALITY_CHOICES, "standard"),
        CAMERA_QUALITY => Kind::Word(&CAMERA_QUALITY_CHOICES, "auto"),
        CAMERA_MIRROR => Kind::Switch(true),
        CAMERA_BLUR => Kind::Switch(false),
        JOIN_MUTED | PUSH_TO_TALK | REQUIRE_CALL_E2EE | LINK_PREVIEWS_UNKNOWN | STREAMER_HIDE_WINDOW | STREAMER_DETECT_OBS => Kind::Switch(false),
        STREAMER_MODE => Kind::Word(&STREAMER_MODE_CHOICES, "while_sharing"),
        MUTED_TALK | AUTO_WATCH_SHARE => Kind::Switch(true),
        PTT_RELEASE_MS => Kind::Choice(&PTT_RELEASE_CHOICES, 200),
        CUE_VOLUME => Kind::Percent(50),
        VOICE_VOLUME | SOUNDBOARD_VOLUME => Kind::Percent(100),
        SOUNDBOARD_FAVORITES => Kind::Keywords,
        INPUT_VOLUME => Kind::Percent(100),
        SENSITIVITY => Kind::Percent(25),
        INPUT | OUTPUT | CAMERA => Kind::Device,
        PTT_KEY | PRIORITY_KEY | MUTE_KEY | DEAFEN_KEY => Kind::Hotkey,
        _ if gain_user(key).is_some() => Kind::Gain,
        KEEP_DAYS => Kind::Choice(&KEEP_DAYS_CHOICES, 90),
        TRANSLATE_TO => Kind::Word(&TRANSLATE_TO_CHOICES, "auto"),
        CACHE_MB => Kind::Choice(&CACHE_MB_CHOICES, 500),
        // Per account; the default is the network's (`typing_default`).
        TYPING | RECEIPTS | PRESENCE_ACTIVITY => Kind::Switch(true),
        TELEGRAM_ALWAYS_RELAY
        | TELEGRAM_DOWNLOAD_PHOTOS
        | TELEGRAM_DOWNLOAD_PRIVATE
        | TELEGRAM_DOWNLOAD_GROUPS
        | TELEGRAM_DOWNLOAD_CHANNELS => Kind::Switch(false),
        LOG_LEVEL => Kind::Word(&LOG_LEVEL_CHOICES, "info"),
        LOG_RING | LOG_EXPORT => Kind::Switch(true),
        CUSTOM_SOUND => Kind::CustomSound,
        TRANSLATE_LANG => Kind::Language,
        _ => return None,
    })
}

/// Whether `id` names a custom sound: sixteen lower-case hexadecimal characters, the start of
/// the file's SHA-256, so a name chosen by the user never reaches a path.
pub fn sound_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `value` is a sound word a picker may store: one of the bundled ones, `none`, and
/// `custom:<id>` for a file the user added.
pub fn sound_word(value: &str) -> bool {
    if let Some(id) = value.strip_prefix("custom:") {
        return sound_id(id);
    }
    SOUND_CHOICES.contains(&value) || RING_CHOICES.contains(&value)
}

/// The scopes a key may be stored in.
fn scoped(key: &str, scope: &str) -> bool {
    match key {
        TYPING => account_of(scope).is_some_and(|account| typing_choice(account.network)),
        RECEIPTS => account_of(scope).is_some_and(|account| matches!(account.network, Network::Teams | Network::Signal | Network::Tiny)),
        PRESENCE_ACTIVITY => account_of(scope).is_some_and(|account| account.network == Network::Teams),
        TELEGRAM_ALWAYS_RELAY
        | TELEGRAM_DOWNLOAD_PHOTOS
        | TELEGRAM_DOWNLOAD_PRIVATE
        | TELEGRAM_DOWNLOAD_GROUPS
        | TELEGRAM_DOWNLOAD_CHANNELS => account_of(scope).is_some_and(|account| account.network == Network::Telegram),
        _ if gain_user(key).is_some() => account_of(scope).is_some(),
        NOTIFY_ABOUT => scope == GLOBAL || account_of(scope).is_some() || conversation_of(scope).is_some(),
        SOUND => account_of(scope).is_some() || conversation_of(scope).is_some(),
        CUSTOM_SOUND => sound_scope(scope).is_some(),
        _ => scope == GLOBAL,
    }
}

/// The id a `sound:<id>` scope names.
pub fn sound_scope(scope: &str) -> Option<&str> {
    scope.strip_prefix(SOUND_SCOPE_PREFIX).filter(|id| sound_id(id))
}

/// The scope one custom sound's row lives in.
pub fn sound_of(id: &str) -> String {
    format!("{SOUND_SCOPE_PREFIX}{id}")
}

/// Where a custom sound's file is: `<id>.wav` in the sounds directory beside the database.
pub fn sound_path(dir: &std::path::Path, id: &str) -> std::path::PathBuf {
    dir.join("sounds").join(format!("{id}.wav"))
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
        Some(Kind::Word(words, _)) if matches!(key, MESSAGE_SOUND | MENTION_SOUND | RING_SOUND | SOUND) => {
            words.contains(&value) || value.strip_prefix("custom:").is_some_and(sound_id) || value == "same"
        }
        // Older builds stored V11 as on/off; still readable so an upgrade keeps the choice.
        Some(Kind::Word(words, _)) if key == NOISE_SUPPRESSION => words.contains(&value) || matches!(value, "on" | "off"),
        Some(Kind::Word(words, _)) => words.contains(&value),
        Some(Kind::Minute(_)) => value.parse::<u32>().is_ok_and(|n| n < 24 * 60),
        Some(Kind::Until) => value == "resume" || value.parse::<u64>().is_ok(),
        Some(Kind::CustomSound) => !value.is_empty() && value.len() <= 48 && !value.chars().any(char::is_control),
        Some(Kind::Keywords) => value.len() <= 500 && !value.chars().any(|c| c.is_control() && c != '\n'),
        Some(Kind::Host) => !value.is_empty() && value.len() <= 253 && !value.chars().any(|c| c.is_control() || c.is_whitespace() || c == '/' || c == '@'),
        Some(Kind::Port) => value.parse::<u16>().is_ok_and(|p| p > 0),
        Some(Kind::User) => value.len() <= 128 && !value.chars().any(|c| c.is_control() || c == '@' || c == ':'),
        Some(Kind::Language) => valid_language_tag(value),
        None => false,
    }
}

/// Validates `auto` or a short BCP-47 language tag (e.g. `en`, `es`, `pt-BR`, `zh-Hans`).
pub fn valid_language_tag(value: &str) -> bool {
    if value == "auto" {
        return true;
    }
    if !(2..=15).contains(&value.len()) {
        return false;
    }
    let mut parts = value.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    parts.all(|sub| (2..=4).contains(&sub.len()) && sub.bytes().all(|b| b.is_ascii_alphanumeric()))
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
    conversation_key_of(scope).map(|key| key.network)
}

/// The full conversation key named by a `conv:<network>:<account>:<id>` scope.
pub fn conversation_key_of(scope: &str) -> Option<Key> {
    let (word, rest) = scope.strip_prefix("conv:")?.split_once(':')?;
    let network = Network::ALL.into_iter().find(|n| network_word(*n) == word)?;
    let (account, id) = rest.rsplit_once(':')?;
    (!account.is_empty() && !id.is_empty()).then(|| Key {
        network,
        account: account.to_owned(),
        id: id.to_owned(),
    })
}

/// The account an `account:` scope names.
pub fn account_of(scope: &str) -> Option<AccountKey> {
    let (word, account) = scope.strip_prefix("account:")?.split_once(':')?;
    let network = Network::ALL.into_iter().find(|n| network_word(*n) == word)?;
    (!account.is_empty()).then(|| AccountKey { network, account: account.to_owned() })
}

/// Whether the user decides if this network is told I am typing. Telegram always sends it, so it
/// has no switch (P1). Signal has one, but it is a local stop only: the phone's setting still
/// decides on top of it, and nothing here is written back to the account.
pub fn typing_choice(network: Network) -> bool {
    matches!(network, Network::Discord | Network::Slack | Network::Teams | Network::Signal | Network::Tiny)
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

    /// Whether Streamer mode is active given whether the user is sharing a screen
    /// and whether a supported broadcast app (OBS / Streamlabs) is running.
    pub fn streamer_active(&self, sharing: bool, obs_running: bool) -> bool {
        match self.word(GLOBAL, STREAMER_MODE) {
            "on" => true,
            "while_sharing" => sharing || (self.on(STREAMER_DETECT_OBS) && obs_running),
            _ => false,
        }
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

    /// P2 for one account. Teams and Signal have a switch; elsewhere it is the network's to
    /// decide. On Signal this is a local stop: the account's own setting is held separately in
    /// the adapter, and stays off until the phone says otherwise.
    pub fn receipts(&self, account: &AccountKey) -> bool {
        self.stored(&account_scope(account), RECEIPTS).is_none_or(|value| value == "on")
    }

    /// Whether a Teams account sends focus+input active presence heartbeats (`user.activity`).
    pub fn presence_activity(&self, account: &AccountKey) -> bool {
        self.stored(&account_scope(account), PRESENCE_ACTIVITY).is_none_or(|value| value == "on")
    }

    /// The in-call sound and gain choices, as mediad takes them. S1 off silences every cue.
    pub fn tuning(&self) -> crate::Tuning {
        let sounds = self.on(SOUNDS);
        crate::Tuning {
            voice: self.number(VOICE_VOLUME).min(100) as u8,
            gate: self.number(SENSITIVITY).min(100) as u8,
            cues: self.number(CUE_VOLUME).min(100) as u8,
            input: self.number(INPUT_VOLUME).min(100) as u8,
            joins: sounds && self.on(CUE_JOINS),
            mine: sounds && self.on(CUE_MINE),
            connection: sounds && self.on(CUE_CONNECTION),
        }
    }

    /// V11: off, standard (WebRTC), high (WebRTC strongest), or enhanced (voice isolation, the default).
    pub fn noise_mode(&self) -> crate::NoiseMode {
        crate::NoiseMode::parse(self.word(GLOBAL, NOISE_SUPPRESSION)).unwrap_or_default()
    }

    /// How clearly to encode on a Tiny call, within the room's bitrate.
    pub fn voice_quality(&self) -> crate::VoiceQuality {
        crate::VoiceQuality::parse(self.word(GLOBAL, VOICE_QUALITY)).unwrap_or_default()
    }

    /// Preferred target language for on-demand message translation (`auto` or a BCP-47 tag).
    pub fn translate_lang(&self) -> String {
        self.stored(GLOBAL, TRANSLATE_LANG).unwrap_or("auto").to_owned()
    }

    /// V23: Auto, HD (720p30), Standard (360p15), or Data saver.
    pub fn camera_quality(&self) -> crate::CameraQuality {
        crate::CameraQuality::parse(self.word(GLOBAL, CAMERA_QUALITY)).unwrap_or_default()
    }

    /// V23–V25: camera quality, self-view mirror, and on-device background blur.
    pub fn camera_settings(&self) -> crate::CameraSettings {
        crate::CameraSettings {
            quality: self.camera_quality(),
            mirror: self.on(CAMERA_MIRROR),
            blur: self.on(CAMERA_BLUR),
        }
    }

    /// M8: target ISO-639-1 language code for message translation (`"auto"` resolves system locale).
    pub fn translate_target(&self) -> String {
        let word = self.word(GLOBAL, TRANSLATE_TO);
        if word != "auto" && !word.is_empty() {
            return crate::normalize_language_tag(word);
        }
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(val) = std::env::var(var) {
                let norm = crate::normalize_language_tag(&val);
                if !val.trim().is_empty() && !val.eq_ignore_ascii_case("C") && !val.to_ascii_uppercase().starts_with("POSIX") {
                    return norm;
                }
            }
        }
        "en".to_owned()
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
            auto_gain: self.on(AUTO_GAIN),
            noise: self.noise_mode(),
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

/// A sound file the user added (S13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomSound {
    pub id: String,
    pub name: String,
}

impl CustomSound {
    /// The word a picker stores for it.
    pub fn word(&self) -> String {
        format!("custom:{}", self.id)
    }
}

impl Values {
    /// S13: every sound file added, by the name shown. An unreadable or duplicate name is left
    /// out, so a damaged row cannot take a picker down.
    pub fn custom_sounds(&self) -> Vec<CustomSound> {
        let mut rows: Vec<CustomSound> = self
            .0
            .iter()
            .filter(|((_, key), _)| key == CUSTOM_SOUND)
            .filter_map(|((scope, _), value)| Some(CustomSound { id: sound_scope(scope)?.to_owned(), name: value.clone() }))
            .filter(|sound| valid(&sound_of(&sound.id), CUSTOM_SOUND, &sound.name))
            .collect();
        rows.sort_by_key(|sound| sound.name.to_lowercase());
        rows.dedup_by(|a, b| a.id == b.id);
        rows.truncate(CUSTOM_SOUND_LIMIT);
        rows
    }

    /// S13: one added sound, if its file is still registered.
    pub fn custom_sound(&self, id: &str) -> Option<CustomSound> {
        self.custom_sounds().into_iter().find(|sound| sound.id == id)
    }

    /// The name to show for a sound word: a bundled one's name, or a custom one's. `None` for
    /// `none`, `same`, and a custom sound that is no longer registered.
    pub fn sound_name(&self, word: &str) -> Option<String> {
        let bundled = match word {
            "pebble" => "Pebble",
            "drop" => "Drop",
            "tap" => "Tap",
            "chime" => "Chime",
            "glass" => "Glass",
            "marimba" => "Marimba",
            "bell" => "Bell",
            "soft_ring" => "Soft ring",
            "classic_ring" => "Classic ring",
            "ripple" => "Ripple",
            _ => "",
        };
        if !bundled.is_empty() {
            return Some(bundled.to_owned());
        }
        self.custom_sound(word.strip_prefix("custom:")?).map(|sound| sound.name)
    }

    /// S14: ring back while I am calling someone.
    pub fn ringback(&self) -> bool {
        self.on(RINGBACK)
    }

    /// Configured notification keywords, trimmed and non-empty.
    pub fn keywords(&self) -> Vec<String> {
        let Some(raw) = self.stored(GLOBAL, NOTIFY_KEYWORDS) else {
            return Vec::new();
        };
        raw.split([',', '\n'])
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The active network proxy configuration.
    pub fn proxy(&self) -> ProxyConfig {
        let mode = match self.word(GLOBAL, PROXY_MODE) {
            "none" => ProxyMode::None,
            "custom" => ProxyMode::Custom,
            _ => ProxyMode::System,
        };
        let scheme = match self.word(GLOBAL, PROXY_SCHEME) {
            "socks5" => ProxyScheme::Socks5,
            _ => ProxyScheme::Http,
        };
        let host = self.stored(GLOBAL, PROXY_HOST).unwrap_or("").to_owned();
        let port = self.stored(GLOBAL, PROXY_PORT).and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
        let user = self.stored(GLOBAL, PROXY_USER).filter(|u| !u.is_empty()).map(str::to_owned);
        ProxyConfig { mode, scheme, host, port, user }
    }
}

/// Network proxy mode (`Settings › General › Network`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProxyMode {
    #[default]
    System,
    None,
    Custom,
}

/// Protocol for a custom proxy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProxyScheme {
    #[default]
    Http,
    Socks5,
}

/// Resolved proxy configuration read from settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
}

impl ProxyConfig {
    /// Returns the proxy URL for `Custom` mode, or an error if `Custom` is incomplete (fail closed).
    pub fn custom_url(&self, password: Option<&str>) -> Result<Option<String>, String> {
        if self.mode != ProxyMode::Custom {
            return Ok(None);
        }
        let host = self.host.trim();
        if host.is_empty() || self.port == 0 {
            return Err("Custom proxy is enabled but host or port is missing.".to_owned());
        }
        let scheme = match self.scheme {
            ProxyScheme::Http => "http",
            ProxyScheme::Socks5 => "socks5",
        };
        let auth = match (self.user.as_deref().filter(|u| !u.is_empty()), password.filter(|p| !p.is_empty())) {
            (Some(u), Some(p)) => format!("{u}:{p}@"),
            (Some(u), None) => format!("{u}@"),
            _ => String::new(),
        };
        let host_fmt = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        Ok(Some(format!("{scheme}://{auth}{host_fmt}:{}", self.port)))
    }
}

/// Whether `text` contains any keyword in `keywords`, case-insensitively and on word boundaries.
pub fn keyword_matches(text: &str, keywords: &[String]) -> bool {
    if text.is_empty() || keywords.is_empty() {
        return false;
    }
    let hay = text.to_lowercase();
    for kw in keywords {
        let needle = kw.trim().to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let mut start = 0;
        while let Some(rel) = hay[start..].find(&needle) {
            let at = start + rel;
            let end = at + needle.len();
            let left_ok = at == 0 || !hay[..at].chars().next_back().is_some_and(char::is_alphanumeric);
            let right_ok = end == hay.len() || !hay[end..].chars().next().is_some_and(char::is_alphanumeric);
            if left_ok && right_ok {
                return true;
            }
            // Advance by one char to find subsequent occurrences.
            let step = hay[at..].chars().next().map_or(1, char::len_utf8);
            start = at + step;
        }
    }
    false
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
        let signal = account_scope(&account(Network::Signal));
        assert!(valid(&signal, TYPING, "off") && valid(&signal, RECEIPTS, "off"), "Signal takes a local switch for both");
        let tiny = account_scope(&account(Network::Tiny));
        assert!(valid(&tiny, TYPING, "off") && valid(&tiny, RECEIPTS, "off"), "Tiny takes a switch for both");
        assert!(valid(GLOBAL, INPUT, "Studio Display Microphone") && !valid(GLOBAL, INPUT, ""));
    }

    #[test]
    fn requiring_end_to_end_encrypted_calls_is_a_switch_that_is_off_until_chosen() {        // Off by default on purpose: a server or an app that cannot encrypt would stop working.
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
        let (discord, slack, telegram, tiny) = (account(Network::Discord), account(Network::Slack), account(Network::Telegram), account(Network::Tiny));
        assert!(!values.typing(&discord) && values.typing(&slack) && values.typing(&telegram) && values.typing(&tiny));
        values.set(&account_scope(&discord), TYPING, Some("on".into()));
        values.set(&account_scope(&slack), TYPING, Some("off".into()));
        values.set(&account_scope(&tiny), TYPING, Some("off".into()));
        values.set(&account_scope(&tiny), RECEIPTS, Some("off".into()));
        assert!(values.typing(&discord) && !values.typing(&slack) && !values.typing(&tiny) && !values.receipts(&tiny));
        assert_eq!(account_of(&account_scope(&slack)), Some(slack));
        assert_eq!(account_of("account:myspace:1"), None);
    }

    #[test]
    fn signal_takes_a_local_switch_that_stops_here_and_never_loosens_the_account() {
        let mut values = Values::default();
        let signal = account(Network::Signal);
        // Nothing stored: both are on, and the phone's own setting still decides above them.
        assert!(values.typing(&signal) && values.receipts(&signal));
        values.set(&account_scope(&signal), TYPING, Some("off".into()));
        values.set(&account_scope(&signal), RECEIPTS, Some("off".into()));
        assert!(!values.typing(&signal) && !values.receipts(&signal), "off here stops the send");
        values.set(&account_scope(&signal), TYPING, None);
        values.set(&account_scope(&signal), RECEIPTS, None);
        assert!(values.typing(&signal) && values.receipts(&signal), "a reset follows the account again");
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
        assert!(!values.processing().noise_suppression() && values.processing().echo_cancellation);
        values.set(GLOBAL, NOISE_SUPPRESSION, Some("enhanced".into()));
        assert_eq!(values.noise_mode(), crate::NoiseMode::Enhanced);
        values.set(GLOBAL, NOISE_SUPPRESSION, Some("on".into()));
        assert_eq!(values.noise_mode(), crate::NoiseMode::Enhanced);
        values.set(GLOBAL, NOISE_SUPPRESSION, Some("standard".into()));
        assert_eq!(values.noise_mode(), crate::NoiseMode::Standard);
        assert_eq!(values.voice_quality(), crate::VoiceQuality::Standard);
        values.set(GLOBAL, VOICE_QUALITY, Some("high".into()));
        assert_eq!(values.voice_quality(), crate::VoiceQuality::High);
        assert_eq!(values.tuning().input, 100);
        values.set(GLOBAL, INPUT_VOLUME, Some("70".into()));
        assert_eq!(values.tuning().input, 70);
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
        let discord = account_scope(&AccountKey { network: Network::Discord, account: "acc".into() });
        assert!(valid(&discord, "gain:u1", "140") && valid(&discord, "gain:u1", "0 muted"));
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
    fn play_sounds_off_silences_every_cue_and_keeps_the_choices() {        let mut values = Values::default();
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

    #[test]
    fn a_custom_sound_is_a_file_id_and_a_name_and_the_pickers_take_it() {
        let id = "0123456789abcdef";
        let scope = sound_of(id);
        assert_eq!(sound_scope(&scope), Some(id));
        assert!(valid(&scope, CUSTOM_SOUND, "My tone"));
        assert!(!valid(&scope, CUSTOM_SOUND, ""), "a sound needs a name");
        assert!(!valid(&scope, CUSTOM_SOUND, "two\nlines"), "and no control characters");
        assert!(!valid(GLOBAL, CUSTOM_SOUND, "My tone"), "only in a sound scope");
        assert!(!valid(&sound_of("nope"), CUSTOM_SOUND, "My tone"), "the id is the file's");
        assert!(!valid(&sound_of("0123456789ABCDEF"), CUSTOM_SOUND, "My tone"), "lower case only");
        assert!(!valid(&sound_of("../etc/passwd"), CUSTOM_SOUND, "My tone"));

        let word = format!("custom:{id}");
        for key in [MESSAGE_SOUND, MENTION_SOUND, RING_SOUND] {
            assert!(valid(GLOBAL, key, &word), "{key}");
        }
        assert!(valid(&account_scope(&account(Network::Slack)), SOUND, &word));
        assert!(!valid(GLOBAL, MENTION_SOUND, "custom:0"), "an id is sixteen characters");
    }

    #[test]
    fn every_added_sound_is_listed_by_name_and_an_unregistered_one_by_nothing() {
        let id = "0123456789abcdef";
        let mut values = Values::default();
        values.set(&sound_of(id), CUSTOM_SOUND, Some("Rain on the roof".into()));
        values.set(&sound_of("fedcba9876543210"), CUSTOM_SOUND, Some("Air horn".into()));
        let named: Vec<String> = values.custom_sounds().into_iter().map(|sound| sound.name).collect();
        assert_eq!(named, ["Air horn", "Rain on the roof"], "sorted, whatever the order they were added");
        assert_eq!(values.sound_name(&format!("custom:{id}")).as_deref(), Some("Rain on the roof"));
        assert_eq!(values.sound_name("custom:ffffffffffffffff"), None);
        assert_eq!(values.sound_name("none"), None);
        assert_eq!(values.sound_name("soft_ring").as_deref(), Some("Soft ring"));
        assert!(!values.custom_sound(id).is_some_and(|sound| sound.word() != format!("custom:{id}")));
    }

    #[test]
    fn ringing_back_while_calling_is_on_until_it_is_turned_off() {
        assert!(Values::default().ringback(), "someone who calls out expects to hear it ring");
        assert!(valid(GLOBAL, RINGBACK, "on") && valid(GLOBAL, RINGBACK, "off"));
        let mut values = Values::default();
        values.set(GLOBAL, RINGBACK, Some("off".into()));
        assert!(!values.ringback());
    }

    #[test]
    fn reactions_followed_threads_and_keywords_settings_validate_and_match_word_boundaries() {
        let mut values = Values::default();
        assert_eq!(values.word(GLOBAL, NOTIFY_REACTIONS), "off");
        assert!(values.on(NOTIFY_THREADS));
        assert!(values.keywords().is_empty());
        for word in ["off", "direct", "everywhere"] {
            assert!(valid(GLOBAL, NOTIFY_REACTIONS, word), "{word}");
        }
        assert!(!valid(GLOBAL, NOTIFY_REACTIONS, "all"));
        assert!(valid(GLOBAL, NOTIFY_THREADS, "off"));
        assert!(valid(GLOBAL, NOTIFY_KEYWORDS, "deploy, incident, Alice"));
        assert!(!valid(GLOBAL, NOTIFY_KEYWORDS, "bad\x07bell"));

        values.set(GLOBAL, NOTIFY_KEYWORDS, Some("deploy, incident,  cat ".into()));
        let kws = values.keywords();
        assert_eq!(kws, ["deploy", "incident", "cat"]);
        assert!(keyword_matches("Time to DEPLOY now!", &kws));
        assert!(keyword_matches("An incident occurred.", &kws));
        assert!(keyword_matches("cat!", &kws));
        assert!(!keyword_matches("catch the ball", &kws), "must match whole word");
        assert!(!keyword_matches("redeploying", &kws), "must match whole word");
        assert!(!keyword_matches("", &kws));
    }

    #[test]
    fn proxy_settings_validate_and_custom_mode_fails_closed_when_incomplete() {
        let mut values = Values::default();
        assert_eq!(values.proxy().mode, ProxyMode::System);
        assert_eq!(values.proxy().custom_url(None), Ok(None));

        for mode in ["system", "none", "custom"] {
            assert!(valid(GLOBAL, PROXY_MODE, mode), "{mode}");
        }
        assert!(!valid(GLOBAL, PROXY_MODE, "direct"));
        assert!(valid(GLOBAL, PROXY_SCHEME, "http") && valid(GLOBAL, PROXY_SCHEME, "socks5"));
        assert!(!valid(GLOBAL, PROXY_SCHEME, "ftp"));
        assert!(valid(GLOBAL, PROXY_HOST, "proxy.corp.local") && !valid(GLOBAL, PROXY_HOST, "http://bad/path"));
        assert!(valid(GLOBAL, PROXY_PORT, "1080") && !valid(GLOBAL, PROXY_PORT, "0") && !valid(GLOBAL, PROXY_PORT, "70000"));
        assert!(valid(GLOBAL, PROXY_USER, "alice") && !valid(GLOBAL, PROXY_USER, "bad:user"));

        // Custom mode without host/port must fail closed.
        values.set(GLOBAL, PROXY_MODE, Some("custom".into()));
        assert!(values.proxy().custom_url(None).is_err(), "incomplete custom proxy must fail closed");

        values.set(GLOBAL, PROXY_SCHEME, Some("socks5".into()));
        values.set(GLOBAL, PROXY_HOST, Some("127.0.0.1".into()));
        values.set(GLOBAL, PROXY_PORT, Some("1080".into()));
        assert_eq!(values.proxy().custom_url(None).unwrap().as_deref(), Some("socks5://127.0.0.1:1080"));

        values.set(GLOBAL, PROXY_USER, Some("alice".into()));
        assert_eq!(
            values.proxy().custom_url(Some("s3cret")).unwrap().as_deref(),
            Some("socks5://alice:s3cret@127.0.0.1:1080")
        );
    }

    #[test]
    fn telegram_always_relay_switch_and_p2p_promise() {
        let tg = account_scope(&account(Network::Telegram));
        let dc = account_scope(&account(Network::Discord));
        assert!(valid(&tg, TELEGRAM_ALWAYS_RELAY, "on"));
        assert!(valid(&tg, TELEGRAM_ALWAYS_RELAY, "off"));
        assert!(!valid(&dc, TELEGRAM_ALWAYS_RELAY, "on"));
        assert!(!valid(GLOBAL, TELEGRAM_ALWAYS_RELAY, "on"));
        // Media must run relay-only when Telegram says p2p_allowed=false OR local always_relay=true.
        assert!(telegram_relay_only(false, false));
        assert!(telegram_relay_only(false, true));
        assert!(telegram_relay_only(true, true));
        assert!(!telegram_relay_only(true, false));
    }

    #[test]
    fn translate_lang_validates_auto_and_bcp47_tags() {
        assert!(valid(GLOBAL, TRANSLATE_LANG, "auto"));
        assert!(valid(GLOBAL, TRANSLATE_LANG, "en"));
        assert!(valid(GLOBAL, TRANSLATE_LANG, "pt-BR"));
        assert!(valid(GLOBAL, TRANSLATE_LANG, "zh-Hans"));
        assert!(!valid(GLOBAL, TRANSLATE_LANG, ""));
        assert!(!valid(GLOBAL, TRANSLATE_LANG, "1"));
        assert!(!valid(GLOBAL, TRANSLATE_LANG, "en US"));
        assert!(!valid(&account_scope(&account(Network::Teams)), TRANSLATE_LANG, "en"));
        let mut values = Values::default();
        assert_eq!(values.translate_lang(), "auto");
        values.set(GLOBAL, TRANSLATE_LANG, Some("es".into()));
        assert_eq!(values.translate_lang(), "es");
    }

    #[test]
    fn auto_watch_share_defaults_on_and_is_global() {
        assert!(Values::default().on(AUTO_WATCH_SHARE));
        assert!(valid(GLOBAL, AUTO_WATCH_SHARE, "on"));
        assert!(valid(GLOBAL, AUTO_WATCH_SHARE, "off"));
        assert!(!valid(&account_scope(&account(Network::Tiny)), AUTO_WATCH_SHARE, "off"));
    }

    #[test]
    fn streamer_mode_defaults_and_activation_rules() {
        let mut values = Values::default();
        assert_eq!(values.word(GLOBAL, STREAMER_MODE), "while_sharing");
        assert!(!values.on(STREAMER_HIDE_WINDOW));
        assert!(!values.on(STREAMER_DETECT_OBS));
        for word in STREAMER_MODE_CHOICES {
            assert!(valid(GLOBAL, STREAMER_MODE, word), "{word}");
        }
        assert!(!valid(GLOBAL, STREAMER_MODE, "always"));
        assert!(valid(GLOBAL, STREAMER_HIDE_WINDOW, "on"));
        assert!(valid(GLOBAL, STREAMER_DETECT_OBS, "on"));
        assert!(!valid(&account_scope(&account(Network::Tiny)), STREAMER_MODE, "on"));

        // Default "while_sharing": active only while sharing (unless OBS detection is enabled).
        assert!(!values.streamer_active(false, false));
        assert!(values.streamer_active(true, false));
        assert!(!values.streamer_active(false, true));

        // Enabling OBS detection activates "while_sharing" when OBS is running.
        values.set(GLOBAL, STREAMER_DETECT_OBS, Some("on".into()));
        assert!(values.streamer_active(false, true));
        assert!(!values.streamer_active(false, false));

        // Explicit "on" is always active; explicit "off" is never active even while sharing or OBS running.
        values.set(GLOBAL, STREAMER_MODE, Some("on".into()));
        assert!(values.streamer_active(false, false));
        values.set(GLOBAL, STREAMER_MODE, Some("off".into()));
        assert!(!values.streamer_active(true, true));
    }

    #[test]
    fn camera_quality_mirror_and_blur_settings_validate_and_resolve() {
        let mut values = Values::default();
        assert_eq!(values.camera_settings(), crate::CameraSettings {
            quality: crate::CameraQuality::Auto,
            mirror: true,
            blur: false,
        });
        for q in CAMERA_QUALITY_CHOICES {
            assert!(valid(GLOBAL, CAMERA_QUALITY, q), "{q}");
        }
        assert!(!valid(GLOBAL, CAMERA_QUALITY, "4k"));
        assert!(valid(GLOBAL, CAMERA_MIRROR, "off") && valid(GLOBAL, CAMERA_MIRROR, "on"));
        assert!(valid(GLOBAL, CAMERA_BLUR, "on") && valid(GLOBAL, CAMERA_BLUR, "off"));
        assert!(valid(GLOBAL, CAMERA, "FaceTime HD\tcam-0"));

        values.set(GLOBAL, CAMERA_QUALITY, Some("hd".into()));
        values.set(GLOBAL, CAMERA_MIRROR, Some("off".into()));
        values.set(GLOBAL, CAMERA_BLUR, Some("on".into()));
        assert_eq!(values.camera_settings(), crate::CameraSettings {
            quality: crate::CameraQuality::Hd,
            mirror: false,
            blur: true,
        });
    }

    #[test]
    fn translate_target_setting_validates_and_resolves() {
        let mut values = Values::default();
        for code in TRANSLATE_TO_CHOICES {
            assert!(valid(GLOBAL, TRANSLATE_TO, code), "{code}");
        }
        assert!(!valid(GLOBAL, TRANSLATE_TO, "klingon"));
        assert!(!valid(&account_scope(&account(Network::Telegram)), TRANSLATE_TO, "es"));
        assert!(!values.translate_target().is_empty());

        values.set(GLOBAL, TRANSLATE_TO, Some("ja".into()));
        assert_eq!(values.translate_target(), "ja");
        values.set(GLOBAL, TRANSLATE_TO, Some("fr".into()));
        assert_eq!(values.translate_target(), "fr");
    }
}
