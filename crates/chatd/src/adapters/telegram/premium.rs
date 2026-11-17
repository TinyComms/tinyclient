//! Telegram Premium, as far as this adapter can show it. The plan is named on our account.
//! Someone else's Premium is named on their row. A send Telegram refuses for lack of Premium
//! says so. Buying stays a hand-off to Telegram's own app.
//!
//! Limits come from `help.getAppConfig` when that answer arrives. Until then, and when a key
//! is missing, the documented defaults stand.

use super::wire::Fault;
use super::Account;
use grammers_tl_types as tl;

/// What a refused Premium-only send says.
pub const NEEDS: &str = "This needs Telegram Premium.";

/// The hand-off for buying Premium. Telegram's own app takes it from here.
pub const OFFER: &str = "tg://premium_offer";

/// Shown under someone else's name when their account is Premium. Not our own status text:
/// that field is what a person sets for others to read.
pub const BADGE: &str = "Telegram Premium";

/// One part of an upload. Telegram counts a file as this many parts (`upload_max_fileparts_*`).
const PART: u64 = 512 * 1024;

/// What the account may do. Premium raises the file size, the caption, and reactions per message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub file_free: u64,
    pub file_premium: u64,
    pub caption_free: u32,
    pub caption_premium: u32,
    pub reactions_free: u32,
    pub reactions_premium: u32,
    pub reactions_distinct: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // 2 GB and 4 GB: 4,000 parts of 512 KB, 8,000 with Premium.
            file_free: 2_000 * 1024 * 1024,
            file_premium: 4_000 * 1024 * 1024,
            caption_free: 1_024,
            caption_premium: 4_096,
            // `reactions_user_max_default`, `reactions_user_max_premium`, `reactions_uniq_max`.
            reactions_free: super::react::MINE,
            reactions_premium: super::react::MINE_PREMIUM,
            reactions_distinct: super::react::DISTINCT,
        }
    }
}

impl Limits {
    pub fn file(&self, premium: bool) -> u64 {
        if premium {
            self.file_premium
        } else {
            self.file_free
        }
    }

    pub fn caption(&self, premium: bool) -> u32 {
        if premium {
            self.caption_premium
        } else {
            self.caption_free
        }
    }

    pub fn reactions(&self, premium: bool) -> u32 {
        if premium {
            self.reactions_premium
        } else {
            self.reactions_free
        }
    }
}

/// A file past the free ceiling and within Premium's. Premium itself is not refused here, and
/// a file past even Premium's ceiling is a different limit.
pub fn file_refusal(bytes: u64, premium: bool, limits: &Limits) -> Option<&'static str> {
    if !premium && bytes > limits.file_free && bytes <= limits.file_premium {
        Some(NEEDS)
    } else {
        None
    }
}

/// The subtitle for a Premium account that is not ours.
pub fn badge(premium: bool, mine: bool) -> &'static str {
    if premium && !mine {
        BADGE
    } else {
        ""
    }
}

/// `help.getAppConfig`'s JSON object. A missing key keeps that limit's default.
pub fn limits_of(config: &tl::enums::Jsonvalue) -> Limits {
    let mut limits = Limits::default();
    let tl::enums::Jsonvalue::JsonObject(object) = config else {
        return limits;
    };
    for entry in &object.value {
        let tl::enums::JsonobjectValue::JsonObjectValue(entry) = entry;
        let Some(number) = json_number(&entry.value) else {
            continue;
        };
        match entry.key.as_str() {
            "caption_length_limit_default" => limits.caption_free = number as u32,
            "caption_length_limit_premium" => limits.caption_premium = number as u32,
            "upload_max_fileparts_default" => {
                limits.file_free = (number as u64).saturating_mul(PART)
            }
            "upload_max_fileparts_premium" => {
                limits.file_premium = (number as u64).saturating_mul(PART)
            }
            "reactions_user_max_default" => limits.reactions_free = number as u32,
            "reactions_user_max_premium" => limits.reactions_premium = number as u32,
            "reactions_uniq_max" => limits.reactions_distinct = number as u32,
            _ => {}
        }
    }
    limits
}

fn json_number(value: &tl::enums::Jsonvalue) -> Option<f64> {
    match value {
        tl::enums::Jsonvalue::JsonNumber(number) => Some(number.value),
        _ => None,
    }
}

/// Best-effort. A missing or unreadable config keeps the documented defaults, so sign-on still
/// finishes when Telegram does not answer this.
pub fn read(account: &Account) -> Limits {
    match account.ask(&tl::functions::help::GetAppConfig { hash: 0 }) {
        Ok(tl::enums::help::AppConfig::Config(config)) => limits_of(&config.config),
        _ => Limits::default(),
    }
}

/// The lab's telegram-premium check. A 3 GiB file on a free account is named as Premium, the
/// refusal words say so, app config can raise one limit, and a gifted Premium still reads.
pub fn prove() -> Result<(), String> {
    let required = Fault::Rpc {
        code: 403,
        name: "PREMIUM_ACCOUNT_REQUIRED".into(),
        value: None,
    };
    if !required.words().contains("needs Telegram Premium") {
        return Err(format!(
            "a Premium refusal is unnamed: {}",
            required.words()
        ));
    }
    let too_much = Fault::Rpc {
        code: 400,
        name: "STICKERS_TOO_MUCH".into(),
        value: None,
    };
    if !too_much.words().contains("needs Telegram Premium") {
        return Err(format!("a size limit is unnamed: {}", too_much.words()));
    }
    let long = Fault::Rpc {
        code: 400,
        name: "MESSAGE_TOO_LONG".into(),
        value: None,
    };
    if long.words().contains("needs Telegram Premium") {
        return Err(format!(
            "a long message was called Premium: {}",
            long.words()
        ));
    }
    let bytes = 3 * 1024 * 1024 * 1024;
    let limits = Limits::default();
    if file_refusal(bytes, false, &limits) != Some(NEEDS) {
        return Err("a 3 GiB file on a free account was not named as Premium".into());
    }
    if file_refusal(bytes, true, &limits).is_some() {
        return Err("Premium was refused a 3 GiB file".into());
    }
    let parsed = limits_of(&sample_config(2_048.0));
    if parsed.caption_free != 2_048 {
        return Err(format!(
            "app config did not set the free caption, got {}",
            parsed.caption_free
        ));
    }
    super::content::prove_service_lines()
}

fn sample_config(caption: f64) -> tl::enums::Jsonvalue {
    tl::types::JsonObject {
        value: vec![tl::types::JsonObjectValue {
            key: "caption_length_limit_default".into(),
            value: tl::types::JsonNumber { value: caption }.into(),
        }
        .into()],
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_between_the_free_and_premium_ceilings_needs_premium() {
        let limits = Limits::default();
        let bytes = 3 * 1024 * 1024 * 1024;
        assert_eq!(file_refusal(bytes, false, &limits), Some(NEEDS));
        assert_eq!(file_refusal(bytes, true, &limits), None);
        assert_eq!(file_refusal(limits.file_free, false, &limits), None);
        assert_eq!(file_refusal(limits.file_premium + 1, false, &limits), None);
    }

    #[test]
    fn app_config_overrides_one_limit_and_leaves_the_rest() {
        let parsed = limits_of(&sample_config(2_048.0));
        assert_eq!(parsed.caption_free, 2_048);
        assert_eq!(parsed.caption_premium, Limits::default().caption_premium);
        assert_eq!(parsed.file_free, Limits::default().file_free);
    }
}
