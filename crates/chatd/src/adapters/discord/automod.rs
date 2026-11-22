//! Discord AutoMod rules and server safety setup (#395).
//!
//! Listing and every write require Manage Server, checked before any HTTP. A block message is
//! the text Discord shows the member; it is never treated as a command. A trigger or action this
//! build does not understand is kept read-only and, if saved, sent back as the original fields.

use super::model::{iso_seconds, seconds_iso, MANAGE_GUILD};
use super::super::{AutomodPage, Refusal};
use super::{refusal, Discord};
use protocol::{AutomodAction, AutomodRule, AutomodTrigger, SpaceSafety};
use serde_json::{json, Value};

const KEYWORD: u8 = 1;
const SPAM: u8 = 3;
const PRESET: u8 = 4;
const MENTION: u8 = 5;
const PROFILE: u8 = 6;
const EVENT_MESSAGE: u8 = 1;
const EVENT_MEMBER: u8 = 2;

const BLOCK: u8 = 1;
const ALERT: u8 = 2;
const TIMEOUT: u8 = 3;
const QUARANTINE: u8 = 4;

const NAME_MAX: usize = 100;
const MESSAGE_MAX: usize = 150;
const KEYWORD_LEN: usize = 60;
const KEYWORD_COUNT: usize = 1000;
const REGEX_LEN: usize = 260;
const REGEX_COUNT: usize = 10;
const ALLOW_KEYWORD: usize = 100;
const ALLOW_PRESET: usize = 1000;
const MENTION_MAX: u32 = 50;
const TIMEOUT_MAX: u32 = 2_419_200;
const EXEMPT_ROLES: usize = 20;
const EXEMPT_CHANNELS: usize = 50;
/// Discord allows a pause at most 24 hours ahead. A minute covers the click.
const INCIDENT_AHEAD: i64 = 24 * 3600 + 60;

const CANNOT_EDIT: &str = "This rule uses a trigger or action tinyclient cannot edit.";
const UNREADABLE: &str = "Discord's answer was unreadable.";

fn is_snowflake(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn require_snowflake(id: &str, what: &str) -> Result<(), Refusal> {
    if is_snowflake(id) {
        Ok(())
    } else {
        Err(Refusal::Refused(format!("{what} is not a Discord id.")))
    }
}

fn snow(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.as_u64().map(|n| n.to_string()).or_else(|| number.as_i64().map(|n| n.to_string())).unwrap_or_default(),
        _ => String::new(),
    }
}

fn as_u8(value: &Value) -> Option<u8> {
    value.as_u64().and_then(|n| u8::try_from(n).ok()).or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn as_u32(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok()).or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn guild_field<'a>(guild: &'a Value, name: &str) -> Option<&'a Value> {
    guild.get(name).or_else(|| guild.get("properties").and_then(|props| props.get(name)))
}

fn level(value: Option<&Value>) -> Result<u8, Refusal> {
    let Some(value) = value else { return Ok(0) };
    if value.is_null() {
        return Ok(0);
    }
    as_u8(value).ok_or_else(|| Refusal::Refused(UNREADABLE.to_owned()))
}

fn incident_until(incidents: Option<&Value>, key: &str) -> Result<i64, Refusal> {
    let Some(incidents) = incidents else { return Ok(0) };
    if incidents.is_null() {
        return Ok(0);
    }
    let Some(obj) = incidents.as_object() else {
        return Err(Refusal::Refused(UNREADABLE.to_owned()));
    };
    match obj.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::String(text)) => iso_seconds(text).ok_or_else(|| Refusal::Refused(UNREADABLE.to_owned())),
        Some(_) => Err(Refusal::Refused(UNREADABLE.to_owned())),
    }
}

fn parse_safety(guild: &Value) -> Result<SpaceSafety, Refusal> {
    if !guild.is_object() {
        return Err(Refusal::Refused(UNREADABLE.to_owned()));
    }
    let incidents = guild_field(guild, "incidents_data");
    Ok(SpaceSafety {
        verification_level: level(guild_field(guild, "verification_level"))?,
        explicit_filter: level(guild_field(guild, "explicit_content_filter"))?,
        invites_paused_until: incident_until(incidents, "invites_disabled_until")?,
        dms_paused_until: incident_until(incidents, "dms_disabled_until")?,
    })
}

fn has_extra(value: &Value, allowed: &[&str]) -> bool {
    match value.as_object() {
        Some(obj) => obj.keys().any(|key| !allowed.contains(&key.as_str())),
        None => !value.is_null(),
    }
}

fn string_list(value: Option<&Value>, max_len: usize, max_count: usize) -> (Vec<String>, bool) {
    let Some(value) = value else { return (Vec::new(), false) };
    if value.is_null() {
        return (Vec::new(), false);
    }
    let Some(items) = value.as_array() else { return (Vec::new(), true) };
    let mut out = Vec::new();
    let mut odd = items.len() > max_count;
    for item in items {
        let Some(text) = item.as_str() else {
            odd = true;
            continue;
        };
        if text.chars().count() > max_len || text.chars().any(char::is_control) {
            odd = true;
        }
        out.push(text.to_owned());
    }
    (out, odd)
}

fn metadata_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

fn parse_trigger(code: u8, metadata: &Value) -> (AutomodTrigger, bool) {
    let meta = metadata_object(metadata);
    let missing = meta.is_none() && !metadata.is_null();
    let meta_value = meta.map(|obj| Value::Object(obj.clone())).unwrap_or_else(|| Value::Object(Default::default()));
    let get = |key: &str| meta.and_then(|obj| obj.get(key));
    match code {
        KEYWORD | PROFILE => {
            let allowed = ["keyword_filter", "regex_patterns", "allow_list"];
            let (keywords, words_odd) = string_list(get("keyword_filter"), KEYWORD_LEN, KEYWORD_COUNT);
            let (regex, regex_odd) = string_list(get("regex_patterns"), REGEX_LEN, REGEX_COUNT);
            let (allow, allow_odd) = string_list(get("allow_list"), KEYWORD_LEN, ALLOW_KEYWORD);
            let odd = missing || has_extra(&meta_value, &allowed) || words_odd || regex_odd || allow_odd;
            let trigger = if code == PROFILE {
                AutomodTrigger::Profile { keywords, regex, allow }
            } else {
                AutomodTrigger::Keyword { keywords, regex, allow }
            };
            (trigger, odd)
        }
        SPAM => {
            let odd = missing || has_extra(&meta_value, &[]);
            (AutomodTrigger::Spam, odd)
        }
        PRESET => {
            let (presets, presets_odd) = preset_list(get("presets"));
            let (allow, allow_odd) = string_list(get("allow_list"), KEYWORD_LEN, ALLOW_PRESET);
            let odd = missing || has_extra(&meta_value, &["presets", "allow_list"]) || presets_odd || allow_odd;
            (AutomodTrigger::Preset { presets, allow }, odd)
        }
        MENTION => {
            let limit = get("mention_total_limit").and_then(as_u32);
            let raid = get("mention_raid_protection_enabled").and_then(Value::as_bool);
            let raid_odd = get("mention_raid_protection_enabled").is_some_and(|value| !value.is_null() && raid.is_none());
            let limit_odd = limit.is_none_or(|limit| !(1..=MENTION_MAX).contains(&limit));
            let odd = missing || has_extra(&meta_value, &["mention_total_limit", "mention_raid_protection_enabled"]) || raid_odd || limit_odd;
            (AutomodTrigger::Mention { limit: limit.unwrap_or(0), raid_protection: raid.unwrap_or(false) }, odd)
        }
        other => {
            let text = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_owned());
            (AutomodTrigger::Other { code: other, metadata: text }, true)
        }
    }
}

fn preset_list(value: Option<&Value>) -> (Vec<u8>, bool) {
    let Some(value) = value else { return (Vec::new(), true) };
    let Some(items) = value.as_array() else { return (Vec::new(), true) };
    let mut presets = Vec::new();
    let mut odd = items.is_empty();
    for item in items {
        match as_u8(item) {
            Some(code @ 1..=3) => presets.push(code),
            Some(code) => {
                odd = true;
                presets.push(code);
            }
            None => odd = true,
        }
    }
    (presets, odd)
}

fn id_list(value: Option<&Value>, max: usize) -> (Vec<String>, bool) {
    match value {
        None | Some(Value::Null) => (Vec::new(), false),
        Some(Value::Array(items)) => {
            let mut ids = Vec::new();
            let mut odd = items.len() > max;
            for item in items {
                let id = snow(item);
                if !is_snowflake(&id) {
                    odd = true;
                }
                ids.push(id);
            }
            (ids, odd)
        }
        Some(_) => (Vec::new(), true),
    }
}

fn action_metadata_text(metadata: &Value) -> String {
    if metadata.is_null() {
        return String::new();
    }
    serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_owned())
}

fn parse_actions(value: &Value, trigger: u8) -> (Vec<AutomodAction>, bool) {
    let Some(items) = value.as_array() else {
        return (Vec::new(), true);
    };
    let mut actions = Vec::new();
    let mut seen = Vec::new();
    let mut odd = items.is_empty();
    for item in items {
        let Some(obj) = item.as_object() else {
            odd = true;
            continue;
        };
        let Some(code) = obj.get("type").and_then(as_u8) else {
            odd = true;
            continue;
        };
        if seen.contains(&code) {
            odd = true;
        }
        seen.push(code);
        let metadata = obj.get("metadata").cloned().unwrap_or(Value::Null);
        let meta = metadata.as_object();
        let get = |key: &str| meta.and_then(|obj| obj.get(key));
        let action = match code {
            BLOCK => {
                if has_extra(&metadata, &["custom_message"]) {
                    odd = true;
                }
                let message = match get("custom_message") {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(text)) => {
                        if text.chars().count() > MESSAGE_MAX || text.chars().any(char::is_control) {
                            odd = true;
                        }
                        text.clone()
                    }
                    Some(_) => {
                        odd = true;
                        String::new()
                    }
                };
                AutomodAction::Block { message }
            }
            ALERT => {
                if has_extra(&metadata, &["channel_id"]) {
                    odd = true;
                }
                let channel_id = get("channel_id").map(snow).unwrap_or_default();
                if !is_snowflake(&channel_id) {
                    odd = true;
                }
                AutomodAction::Alert { channel_id }
            }
            TIMEOUT => {
                if has_extra(&metadata, &["duration_seconds"]) || (trigger != KEYWORD && trigger != MENTION) {
                    odd = true;
                }
                let seconds = get("duration_seconds").and_then(as_u32).unwrap_or(0);
                if !(1..=TIMEOUT_MAX).contains(&seconds) {
                    odd = true;
                }
                AutomodAction::Timeout { seconds }
            }
            QUARANTINE => {
                if has_extra(&metadata, &[]) {
                    odd = true;
                }
                AutomodAction::Quarantine
            }
            other => {
                odd = true;
                AutomodAction::Other { code: other, metadata: action_metadata_text(&metadata) }
            }
        };
        actions.push(action);
    }
    (actions, odd)
}

fn event_for(trigger: &AutomodTrigger) -> u8 {
    match trigger {
        AutomodTrigger::Profile { .. } => EVENT_MEMBER,
        _ => EVENT_MESSAGE,
    }
}

/// One Discord rule object into the protocol. Unknown or out-of-range pieces stay in `raw`.
fn parse_rule(value: &Value) -> Result<AutomodRule, Refusal> {
    let Some(obj) = value.as_object() else {
        return Err(Refusal::Refused(UNREADABLE.to_owned()));
    };
    let id = obj.get("id").map(snow).unwrap_or_default();
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
    let enabled = match obj.get("enabled") {
        Some(Value::Bool(on)) => *on,
        None | Some(Value::Null) => true,
        Some(_) => true,
    };
    let enabled_odd = obj.get("enabled").is_some_and(|value| !value.is_null() && !value.is_boolean());
    let event_type = obj.get("event_type").and_then(as_u8).unwrap_or(EVENT_MESSAGE);
    let trigger_type = obj.get("trigger_type").and_then(as_u8).unwrap_or(0);
    let metadata = obj.get("trigger_metadata").cloned().unwrap_or(Value::Null);
    let (trigger, trigger_odd) = parse_trigger(trigger_type, &metadata);
    let actions_value = obj.get("actions").cloned().unwrap_or(Value::Null);
    let (actions, actions_odd) = parse_actions(&actions_value, trigger_type);
    let (exempt_roles, roles_odd) = id_list(obj.get("exempt_roles"), EXEMPT_ROLES);
    let (exempt_channels, channels_odd) = id_list(obj.get("exempt_channels"), EXEMPT_CHANNELS);
    let name_odd = name.trim().is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control);
    let read_only = trigger_odd || actions_odd || roles_odd || channels_odd || name_odd || enabled_odd;
    let raw = if read_only { serde_json::to_string(value).unwrap_or_default() } else { String::new() };
    let event_type = if read_only { event_type } else { event_for(&trigger) };
    Ok(AutomodRule {
        id,
        name,
        enabled,
        event_type,
        trigger,
        actions,
        exempt_roles,
        exempt_channels,
        read_only,
        raw,
    })
}

fn display_message(text: &str) -> Result<String, Refusal> {
    let trimmed = text.trim();
    if trimmed.chars().any(char::is_control) {
        return Err(Refusal::Refused("A block message cannot contain a newline or a control character.".to_owned()));
    }
    if trimmed.chars().count() > MESSAGE_MAX {
        return Err(Refusal::Refused(format!("A block message can be at most {MESSAGE_MAX} characters.")));
    }
    Ok(trimmed.to_owned())
}

fn word_list(items: &[String], max_len: usize, max_count: usize, what: &str) -> Result<Vec<String>, Refusal> {
    let mut out = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().any(char::is_control) {
            return Err(Refusal::Refused(format!("A {what} cannot contain a control character.")));
        }
        if trimmed.chars().count() > max_len {
            return Err(Refusal::Refused(format!("Each {what} can be at most {max_len} characters.")));
        }
        out.push(trimmed.to_owned());
    }
    if out.len() > max_count {
        return Err(Refusal::Refused(format!("A rule can list at most {max_count} {what}s.")));
    }
    Ok(out)
}

fn snow_list(items: &[String], max: usize, what: &str) -> Result<Vec<String>, Refusal> {
    if items.len() > max {
        return Err(Refusal::Refused(format!("A rule can exempt at most {max}.")));
    }
    let mut out = Vec::new();
    for item in items {
        let id = item.trim();
        if id.is_empty() {
            continue;
        }
        require_snowflake(id, what)?;
        out.push(id.to_owned());
    }
    Ok(out)
}

fn trigger_metadata(trigger: &AutomodTrigger) -> Result<Value, Refusal> {
    match trigger {
        AutomodTrigger::Keyword { keywords, regex, allow } | AutomodTrigger::Profile { keywords, regex, allow } => {
            let keywords = word_list(keywords, KEYWORD_LEN, KEYWORD_COUNT, "keyword")?;
            let regex = word_list(regex, REGEX_LEN, REGEX_COUNT, "pattern")?;
            // Discord rejects a keyword or profile rule whose word and pattern lists are both empty.
            if keywords.is_empty() && regex.is_empty() {
                return Err(Refusal::Refused("A keyword or profile rule needs a keyword or a pattern.".to_owned()));
            }
            Ok(json!({
                "keyword_filter": keywords,
                "regex_patterns": regex,
                "allow_list": word_list(allow, KEYWORD_LEN, ALLOW_KEYWORD, "allowed word")?,
            }))
        }
        AutomodTrigger::Spam => Ok(json!({})),
        AutomodTrigger::Preset { presets, allow } => {
            let mut codes = Vec::new();
            for code in presets {
                if !matches!(code, 1 | 2 | 3) {
                    return Err(Refusal::Refused("A preset filter is profanity, sexual content, or slurs.".to_owned()));
                }
                if !codes.contains(code) {
                    codes.push(*code);
                }
            }
            if codes.is_empty() {
                return Err(Refusal::Refused("Pick at least one preset.".to_owned()));
            }
            Ok(json!({
                "presets": codes,
                "allow_list": word_list(allow, KEYWORD_LEN, ALLOW_PRESET, "allowed word")?,
            }))
        }
        AutomodTrigger::Mention { limit, raid_protection } => {
            if !(1..=MENTION_MAX).contains(limit) {
                return Err(Refusal::Refused("A mention limit is from 1 to 50.".to_owned()));
            }
            Ok(json!({
                "mention_total_limit": limit,
                "mention_raid_protection_enabled": raid_protection,
            }))
        }
        AutomodTrigger::Other { .. } => Err(Refusal::Refused(CANNOT_EDIT.to_owned())),
    }
}

fn action_bodies(actions: &[AutomodAction], trigger: u8) -> Result<Vec<Value>, Refusal> {
    if actions.is_empty() {
        return Err(Refusal::Refused("A rule needs at least one action.".to_owned()));
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for action in actions {
        let code = match action {
            AutomodAction::Block { .. } => BLOCK,
            AutomodAction::Alert { .. } => ALERT,
            AutomodAction::Timeout { .. } => TIMEOUT,
            AutomodAction::Quarantine => QUARANTINE,
            AutomodAction::Other { .. } => return Err(Refusal::Refused(CANNOT_EDIT.to_owned())),
        };
        if seen.contains(&code) {
            return Err(Refusal::Refused("A rule can use each action only once.".to_owned()));
        }
        seen.push(code);
        let body = match action {
            AutomodAction::Block { message } => {
                if trigger == PROFILE {
                    return Err(Refusal::Refused("A block message is not valid on a profile rule.".to_owned()));
                }
                // Display text only. It is never a command and never a content field.
                let message = display_message(message)?;
                let metadata = if message.is_empty() { json!({}) } else { json!({"custom_message": message}) };
                json!({"type": BLOCK, "metadata": metadata})
            }
            AutomodAction::Alert { channel_id } => {
                require_snowflake(channel_id.trim(), "An alert channel")?;
                json!({"type": ALERT, "metadata": {"channel_id": channel_id.trim()}})
            }
            AutomodAction::Timeout { seconds } => {
                if trigger != KEYWORD && trigger != MENTION {
                    return Err(Refusal::Refused("A timeout action is only for keyword and mention rules.".to_owned()));
                }
                if !(1..=TIMEOUT_MAX).contains(seconds) {
                    return Err(Refusal::Refused("A timeout is between 1 second and 4 weeks.".to_owned()));
                }
                json!({"type": TIMEOUT, "metadata": {"duration_seconds": seconds}})
            }
            AutomodAction::Quarantine => {
                if trigger != PROFILE {
                    return Err(Refusal::Refused("Blocking a member is only for a profile rule.".to_owned()));
                }
                json!({"type": QUARANTINE, "metadata": {}})
            }
            AutomodAction::Other { .. } => return Err(Refusal::Refused(CANNOT_EDIT.to_owned())),
        };
        out.push(body);
    }
    Ok(out)
}

fn trigger_code(trigger: &AutomodTrigger) -> Result<u8, Refusal> {
    Ok(match trigger {
        AutomodTrigger::Keyword { .. } => KEYWORD,
        AutomodTrigger::Spam => SPAM,
        AutomodTrigger::Preset { .. } => PRESET,
        AutomodTrigger::Mention { .. } => MENTION,
        AutomodTrigger::Profile { .. } => PROFILE,
        AutomodTrigger::Other { .. } => return Err(Refusal::Refused(CANNOT_EDIT.to_owned())),
    })
}

/// Documented editable keys of an unknown rule, copied unchanged. Identity fields stay off the wire.
fn untouched_body(rule: &AutomodRule) -> Result<Value, Refusal> {
    if rule.raw.is_empty() {
        return Err(Refusal::Refused(CANNOT_EDIT.to_owned()));
    }
    let value: Value = serde_json::from_str(&rule.raw).map_err(|_| Refusal::Refused(CANNOT_EDIT.to_owned()))?;
    let Some(obj) = value.as_object() else {
        return Err(Refusal::Refused(CANNOT_EDIT.to_owned()));
    };
    const KEYS: &[&str] = &[
        "name",
        "event_type",
        "trigger_type",
        "trigger_metadata",
        "actions",
        "enabled",
        "exempt_roles",
        "exempt_channels",
    ];
    let mut out = serde_json::Map::new();
    for key in KEYS {
        if let Some(value) = obj.get(*key) {
            out.insert((*key).to_owned(), value.clone());
        }
    }
    if out.is_empty() {
        return Err(Refusal::Refused(CANNOT_EDIT.to_owned()));
    }
    Ok(Value::Object(out))
}

fn editable_body(rule: &AutomodRule) -> Result<Value, Refusal> {
    if rule.read_only {
        return untouched_body(rule);
    }
    if matches!(rule.trigger, AutomodTrigger::Other { .. }) || rule.actions.iter().any(|action| matches!(action, AutomodAction::Other { .. })) {
        return Err(Refusal::Refused(CANNOT_EDIT.to_owned()));
    }
    let name = rule.name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control) {
        return Err(Refusal::Refused("A rule name is 1 to 100 characters.".to_owned()));
    }
    let code = trigger_code(&rule.trigger)?;
    // Discord's modify endpoint does not accept trigger_type. A create still sends it.
    let mut body = json!({
        "name": name,
        "event_type": event_for(&rule.trigger),
        "trigger_metadata": trigger_metadata(&rule.trigger)?,
        "actions": action_bodies(&rule.actions, code)?,
        "enabled": rule.enabled,
        "exempt_roles": snow_list(&rule.exempt_roles, EXEMPT_ROLES, "An exempt role")?,
        "exempt_channels": snow_list(&rule.exempt_channels, EXEMPT_CHANNELS, "An exempt channel")?,
    });
    if rule.id.is_empty() {
        body.as_object_mut().expect("rule body").insert("trigger_type".to_owned(), json!(code));
    }
    Ok(body)
}

fn parse_saved(raw: &str, sent: &AutomodRule) -> Result<AutomodRule, Refusal> {
    if raw.trim().is_empty() {
        return Ok(sent.clone());
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| Refusal::Refused(UNREADABLE.to_owned()))?;
    if value.is_null() {
        return Ok(sent.clone());
    }
    parse_rule(&value)
}

fn safety_patch(safety: &SpaceSafety) -> Value {
    json!({
        "verification_level": safety.verification_level,
        "explicit_content_filter": safety.explicit_filter,
    })
}

fn until_value(until: i64) -> Value {
    if until <= 0 { Value::Null } else { Value::String(seconds_iso(until)) }
}

fn incident_body(safety: &SpaceSafety) -> Value {
    json!({
        "invites_disabled_until": until_value(safety.invites_paused_until),
        "dms_disabled_until": until_value(safety.dms_paused_until),
    })
}

fn check_until(until: i64, now: i64, what: &str) -> Result<(), Refusal> {
    if until == 0 {
        return Ok(());
    }
    if until < now {
        return Err(Refusal::Refused(format!("{what} can only be paused until a time that has not passed.")));
    }
    if until > now.saturating_add(INCIDENT_AHEAD) {
        return Err(Refusal::Refused(format!("{what} can only be paused for up to 24 hours.")));
    }
    Ok(())
}

fn check_safety(safety: &SpaceSafety, now: i64) -> Result<(), Refusal> {
    if safety.verification_level > 4 {
        return Err(Refusal::Refused("Verification level must be from none to highest.".to_owned()));
    }
    if safety.explicit_filter > 2 {
        return Err(Refusal::Refused("The explicit-content filter must be off, members without roles, or everyone.".to_owned()));
    }
    check_until(safety.invites_paused_until, now, "Invites")?;
    check_until(safety.dms_paused_until, now, "Direct messages")?;
    Ok(())
}

impl Discord {
    fn permit(&self, guild: &str) -> Result<(), Refusal> {
        require_snowflake(guild, "That server")?;
        self.require_guild_perm(guild, MANAGE_GUILD, "manage AutoMod and safety setup")
    }

    /// `GET` the rules, then the guild, so a failed read cannot be saved back as zeros.
    pub(super) fn automod_rules(&self, guild: &str) -> Result<AutomodPage, Refusal> {
        self.permit(guild)?;
        let rules_raw = self.call("GET", &format!("/guilds/{guild}/auto-moderation/rules"), None).map_err(refusal)?;
        let rules_value: Value = serde_json::from_str(&rules_raw).map_err(|_| Refusal::Refused(UNREADABLE.to_owned()))?;
        let Some(items) = rules_value.as_array() else {
            return Err(Refusal::Refused(UNREADABLE.to_owned()));
        };
        let mut rules = Vec::new();
        for item in items {
            rules.push(parse_rule(item)?);
        }
        let guild_raw = self.call("GET", &format!("/guilds/{guild}"), None).map_err(refusal)?;
        let guild_value: Value = serde_json::from_str(&guild_raw).map_err(|_| Refusal::Refused(UNREADABLE.to_owned()))?;
        let safety = parse_safety(&guild_value)?;
        Ok(AutomodPage { rules, safety })
    }

    pub(super) fn save_automod_rule(&self, guild: &str, rule: &AutomodRule) -> Result<AutomodRule, Refusal> {
        self.permit(guild)?;
        if !rule.id.is_empty() {
            require_snowflake(&rule.id, "That rule")?;
        }
        let body = editable_body(rule)?;
        let raw = if rule.id.is_empty() {
            self.call("POST", &format!("/guilds/{guild}/auto-moderation/rules"), Some(body)).map_err(refusal)?
        } else {
            self.call("PATCH", &format!("/guilds/{guild}/auto-moderation/rules/{}", rule.id), Some(body)).map_err(refusal)?
        };
        parse_saved(&raw, rule)
    }

    pub(super) fn delete_automod_rule(&self, guild: &str, id: &str) -> Result<(), Refusal> {
        self.permit(guild)?;
        require_snowflake(id, "That rule")?;
        self.call("DELETE", &format!("/guilds/{guild}/auto-moderation/rules/{id}"), None).map_err(refusal)?;
        Ok(())
    }

    /// One click: the guild fields, then the incident-action pauses.
    pub(super) fn save_space_safety(&self, guild: &str, safety: &SpaceSafety) -> Result<(), Refusal> {
        self.permit(guild)?;
        check_safety(safety, crate::hub::now())?;
        self.call("PATCH", &format!("/guilds/{guild}"), Some(safety_patch(safety))).map_err(refusal)?;
        self.call("PUT", &format!("/guilds/{guild}/incident-actions"), Some(incident_body(safety))).map_err(refusal)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyword_rule(message: &str) -> AutomodRule {
        AutomodRule {
            id: "9101".into(),
            name: "No finance talk".into(),
            enabled: true,
            event_type: 1,
            trigger: AutomodTrigger::Keyword {
                keywords: vec!["invoice".into()],
                regex: vec![r"\d{4}".into()],
                allow: vec!["invoice number".into()],
            },
            actions: vec![
                AutomodAction::Block { message: message.into() },
                AutomodAction::Alert { channel_id: "4101".into() },
                AutomodAction::Timeout { seconds: 60 },
            ],
            exempt_roles: vec!["4010".into()],
            exempt_channels: vec!["4102".into()],
            read_only: false,
            raw: String::new(),
        }
    }

    #[test]
    fn keyword_body_keeps_block_text_as_display_only() {
        let body = editable_body(&keyword_rule("Keep it in #finance")).unwrap();
        assert_eq!(body["trigger_metadata"]["keyword_filter"], json!(["invoice"]));
        assert_eq!(body["actions"][0]["metadata"]["custom_message"], "Keep it in #finance");
        assert_eq!(body["actions"][1]["metadata"]["channel_id"], "4101");
        assert_eq!(body["actions"][2]["metadata"]["duration_seconds"], 60);
        let text = body.to_string();
        assert!(!text.contains("\"command\""));
        assert!(!text.contains("\"content\""));
        assert!(body.get("id").is_none());
    }

    #[test]
    fn a_block_message_with_a_newline_or_too_many_characters_is_refused() {
        let newline = editable_body(&keyword_rule("line\n/bin/sh"));
        assert!(matches!(newline, Err(Refusal::Refused(reason)) if reason.contains("control")));
        let long = editable_body(&keyword_rule(&"x".repeat(151)));
        assert!(matches!(long, Err(Refusal::Refused(reason)) if reason.contains("150")));
    }

    #[test]
    fn actions_and_word_lists_stay_on_what_discord_accepts() {
        let profile = |actions: Vec<AutomodAction>| AutomodRule {
            name: "Names".into(),
            enabled: true,
            trigger: AutomodTrigger::Profile { keywords: vec!["spam".into()], regex: Vec::new(), allow: Vec::new() },
            actions,
            ..AutomodRule::default()
        };
        let blocked = profile(vec![AutomodAction::Block { message: String::new() }]);
        assert!(matches!(editable_body(&blocked), Err(Refusal::Refused(reason)) if reason.contains("block message")));
        let held = editable_body(&profile(vec![AutomodAction::Quarantine])).unwrap();
        assert_eq!(held["trigger_type"], 6);
        assert_eq!(held["actions"][0]["type"], 4);

        let keyword = AutomodRule {
            name: "Words".into(),
            enabled: true,
            trigger: AutomodTrigger::Keyword { keywords: vec!["spam".into()], regex: Vec::new(), allow: Vec::new() },
            actions: vec![AutomodAction::Quarantine],
            ..AutomodRule::default()
        };
        assert!(matches!(editable_body(&keyword), Err(Refusal::Refused(reason)) if reason.contains("member")));

        let edit = editable_body(&keyword_rule("Stay")).unwrap();
        assert!(edit.get("trigger_type").is_none());

        let empty = AutomodRule {
            name: "Words".into(),
            enabled: true,
            trigger: AutomodTrigger::Keyword { keywords: vec![], regex: vec![], allow: vec!["ok".into()] },
            actions: vec![AutomodAction::Block { message: String::new() }],
            ..AutomodRule::default()
        };
        assert!(matches!(editable_body(&empty), Err(Refusal::Refused(reason)) if reason.contains("pattern")));
        let blank_pattern = AutomodRule {
            name: "Names".into(),
            enabled: true,
            trigger: AutomodTrigger::Profile { keywords: vec![], regex: vec![" ".into()], allow: vec![] },
            actions: vec![AutomodAction::Quarantine],
            ..AutomodRule::default()
        };
        assert!(matches!(editable_body(&blank_pattern), Err(Refusal::Refused(reason)) if reason.contains("pattern")));
        let regex_only = AutomodRule {
            name: "Names".into(),
            enabled: true,
            trigger: AutomodTrigger::Profile { keywords: vec![], regex: vec!["spam".into()], allow: vec![] },
            actions: vec![AutomodAction::Quarantine],
            ..AutomodRule::default()
        };
        assert_eq!(editable_body(&regex_only).unwrap()["trigger_metadata"]["regex_patterns"], json!(["spam"]));
    }

    #[test]
    fn a_timeout_on_spam_is_refused() {
        let rule = AutomodRule {
            name: "Spam".into(),
            enabled: true,
            trigger: AutomodTrigger::Spam,
            actions: vec![AutomodAction::Timeout { seconds: 60 }],
            ..AutomodRule::default()
        };
        assert!(matches!(editable_body(&rule), Err(Refusal::Refused(reason)) if reason.contains("timeout")));
    }

    #[test]
    fn an_unknown_rule_is_saved_untouched_and_a_long_keyword_is_not_cut() {
        let raw = json!({
            "id": "9107",
            "guild_id": "4000",
            "creator_id": "1000",
            "name": "Mystery",
            "event_type": 1,
            "trigger_type": 7,
            "trigger_metadata": {"mystery": "leave-me", "script": "rm -rf"},
            "actions": [{"type": 9, "metadata": {"command": "echo pwned", "custom_message": "nope"}}],
            "enabled": true,
            "exempt_roles": [],
            "exempt_channels": []
        });
        let rule = parse_rule(&raw).unwrap();
        assert!(rule.read_only);
        assert!(rule.raw.contains("mystery"));
        let body = editable_body(&rule).unwrap();
        assert_eq!(body["trigger_metadata"]["mystery"], "leave-me");
        assert_eq!(body["actions"][0]["metadata"]["command"], "echo pwned");
        assert!(body.get("command").is_none());
        assert!(body.get("guild_id").is_none());
        assert!(body.get("creator_id").is_none());

        let long = "a".repeat(61);
        let wide = parse_rule(&json!({
            "id": "1",
            "name": "Words",
            "enabled": true,
            "event_type": 1,
            "trigger_type": 1,
            "trigger_metadata": {"keyword_filter": [long], "regex_patterns": [], "allow_list": []},
            "actions": [{"type": 1, "metadata": {}}],
            "exempt_roles": [],
            "exempt_channels": []
        }))
        .unwrap();
        assert!(wide.read_only);
        match wide.trigger {
            AutomodTrigger::Keyword { keywords, .. } => assert_eq!(keywords[0].chars().count(), 61),
            other => panic!("keyword trigger, got {other:?}"),
        }
    }

    #[test]
    fn extra_metadata_is_read_only_and_known_triggers_parse() {
        let extra = parse_rule(&json!({
            "id": "1",
            "name": "Words",
            "enabled": true,
            "event_type": 1,
            "trigger_type": 1,
            "trigger_metadata": {"keyword_filter": ["a"], "regex_patterns": ["(unclosed"], "allow_list": [], "extra": true},
            "actions": [{"type": 1, "metadata": {}}],
            "exempt_roles": [],
            "exempt_channels": []
        }))
        .unwrap();
        assert!(extra.read_only);
        match extra.trigger {
            AutomodTrigger::Keyword { regex, .. } => assert_eq!(regex, ["(unclosed"]),
            other => panic!("keyword trigger, got {other:?}"),
        }

        let mention = parse_rule(&json!({
            "id": "2",
            "name": "Mentions",
            "enabled": true,
            "event_type": 1,
            "trigger_type": 5,
            "trigger_metadata": {"mention_total_limit": 5, "mention_raid_protection_enabled": true},
            "actions": [{"type": 1, "metadata": {}}],
            "exempt_roles": [],
            "exempt_channels": []
        }))
        .unwrap();
        assert!(!mention.read_only);
        assert!(matches!(mention.trigger, AutomodTrigger::Mention { limit: 5, raid_protection: true }));

        let preset = parse_rule(&json!({
            "id": "3",
            "name": "Presets",
            "enabled": true,
            "event_type": 1,
            "trigger_type": 4,
            "trigger_metadata": {"presets": [1, 3], "allow_list": ["ok"]},
            "actions": [{"type": 1, "metadata": {}}],
            "exempt_roles": [],
            "exempt_channels": []
        }))
        .unwrap();
        assert!(matches!(preset.trigger, AutomodTrigger::Preset { presets, .. } if presets == [1, 3]));
    }

    #[test]
    fn safety_patch_sends_only_the_two_guild_fields_and_clears_pauses_with_null() {
        let safety = SpaceSafety { verification_level: 2, explicit_filter: 1, ..SpaceSafety::default() };
        let patch = safety_patch(&safety);
        assert_eq!(patch.as_object().unwrap().len(), 2);
        assert_eq!(patch["verification_level"], 2);
        assert_eq!(patch["explicit_content_filter"], 1);
        let cleared = incident_body(&SpaceSafety::default());
        assert!(cleared["invites_disabled_until"].is_null());
        assert!(cleared["dms_disabled_until"].is_null());

        let nested = json!({
            "properties": {
                "verification_level": 4,
                "explicit_content_filter": 2,
                "incidents_data": {"invites_disabled_until": "2016-04-30T11:18:25.000Z", "dms_disabled_until": null}
            }
        });
        let read = parse_safety(&nested).unwrap();
        assert_eq!(read.verification_level, 4);
        assert_eq!(read.explicit_filter, 2);
        assert_eq!(read.invites_paused_until, 1_462_015_105);
        assert_eq!(read.dms_paused_until, 0);
        assert!(check_safety(&SpaceSafety { verification_level: 9, ..SpaceSafety::default() }, 1_000).is_err());
        assert!(require_snowflake("../x", "That server").is_err());
    }
}
