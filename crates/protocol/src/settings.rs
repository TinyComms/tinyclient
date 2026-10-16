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
/// V1, V2. A device by the name the system gives it; absent follows the system default.
pub const INPUT: &str = "input";
pub const OUTPUT: &str = "output";
/// V4. Every join and every Accept starts muted.
pub const JOIN_MUTED: &str = "join_muted";
/// V5. Everyone's voice, 0 to 100.
pub const VOICE_VOLUME: &str = "voice_volume";
/// V6. The level, 0 to 100, from which our microphone counts as speech.
pub const SENSITIVITY: &str = "sensitivity";
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

pub const KEEP_DAYS_CHOICES: [u32; 3] = [7, 30, 90];
pub const CACHE_MB_CHOICES: [u32; 4] = [250, 500, 1000, 2000];

/// What a key's values look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Switch(bool),
    Percent(u8),
    Choice(&'static [u32], u32),
    Device,
}

fn kind(key: &str) -> Option<Kind> {
    Some(match key {
        KEEP_RUNNING | SOUNDS | CUE_JOINS | CUE_MINE | CUE_CONNECTION => Kind::Switch(true),
        JOIN_MUTED => Kind::Switch(false),
        CUE_VOLUME => Kind::Percent(50),
        VOICE_VOLUME => Kind::Percent(100),
        SENSITIVITY => Kind::Percent(25),
        INPUT | OUTPUT => Kind::Device,
        KEEP_DAYS => Kind::Choice(&KEEP_DAYS_CHOICES, 90),
        CACHE_MB => Kind::Choice(&CACHE_MB_CHOICES, 500),
        // Per account; the default is the network's (`typing_default`).
        TYPING | RECEIPTS => Kind::Switch(true),
        _ => return None,
    })
}

/// The scopes a key may be stored in.
fn scoped(key: &str, scope: &str) -> bool {
    match key {
        TYPING => account_of(scope).is_some_and(|account| typing_choice(account.network)),
        RECEIPTS => account_of(scope).is_some_and(|account| account.network == Network::Teams),
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
        Some(Kind::Device) => !value.is_empty() && value.len() <= 200 && !value.contains(['\n', '\0']),
        None => false,
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
    }
}

pub fn account_scope(account: &AccountKey) -> String {
    format!("account:{}:{}", network_word(account.network), account.account)
}

pub fn conversation_scope(conversation: &Key) -> String {
    format!("conv:{}:{}:{}", network_word(conversation.network), conversation.account, conversation.id)
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

    pub fn device(&self, key: &str) -> Option<String> {
        self.stored(GLOBAL, key).map(str::to_owned)
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
}
