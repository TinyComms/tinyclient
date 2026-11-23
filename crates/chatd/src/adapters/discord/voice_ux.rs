//! Discord activities, stages, and soundboard sounds: the payloads, and nothing that talks
//! to the network. The adapter calls these and the lab's prove hook checks them.

use protocol::{SoundboardSound, StageVoiceAction, VoiceActivity};
use serde_json::{json, Value};

/// One embedded-activity update. `application_id` absent means the activity ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityUpdate {
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub application_id: Option<String>,
    pub users: Vec<String>,
}

/// A stage instance. `ended` is a delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageUpdate {
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub topic: String,
    pub ended: bool,
}

/// A soundboard sound someone played in a voice channel.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectUpdate {
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub user_id: String,
    pub sound_id: String,
    pub emoji: String,
    /// 0 to 1. A listing that sent a percent above 1 is scaled down.
    pub volume: f32,
}

pub fn parse_activity(data: &str) -> Option<ActivityUpdate> {
    let value: Value = serde_json::from_str(data).ok()?;
    let channel_id = text(&value, "channel_id")?;
    let guild_id = value.get("guild_id").and_then(|v| v.as_str()).map(str::to_owned);
    let embedded = value.get("embedded_activity").filter(|v| !v.is_null());
    let embedded = embedded.or_else(|| {
        value.get("embedded_activities").and_then(|v| v.as_array()).and_then(|list| list.first())
    });
    let Some(embedded) = embedded else {
        return Some(ActivityUpdate { channel_id, guild_id, application_id: None, users: Vec::new() });
    };
    let application_id = embedded.get("application_id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned);
    let users = user_ids(embedded);
    if application_id.is_none() {
        return Some(ActivityUpdate { channel_id, guild_id, application_id: None, users: Vec::new() });
    }
    Some(ActivityUpdate { channel_id, guild_id, application_id, users })
}

pub fn parse_applications(body: &str) -> Vec<(String, String, Option<String>)> {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let list = value.as_array().cloned().or_else(|| value.get("applications").and_then(|v| v.as_array()).cloned()).unwrap_or_default();
    list.into_iter().filter_map(|app| {
        let id = app.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())?;
        let name = app.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or("Activity");
        let icon = app.get("icon").and_then(Value::as_str).filter(|icon| !icon.is_empty()).map(str::to_owned);
        Some((id.to_owned(), name.to_owned(), icon))
    }).collect()
}

pub fn app_icon_url(id: &str, icon: &str) -> String {
    format!("https://cdn.discordapp.com/app-icons/{id}/{icon}.png")
}

pub fn activity_of(update: &ActivityUpdate, name: &str, icon: Option<protocol::MediaFile>, handoff: String) -> Option<VoiceActivity> {
    let application_id = update.application_id.clone()?;
    Some(VoiceActivity { application_id, name: name.to_owned(), icon, participants: update.users.clone(), handoff })
}

pub fn parse_stage(data: &str, ended: bool) -> Option<StageUpdate> {
    let value: Value = serde_json::from_str(data).ok()?;
    let channel_id = text(&value, "channel_id")?;
    let guild_id = value.get("guild_id").and_then(Value::as_str).map(str::to_owned);
    let topic = if ended { String::new() } else { value.get("topic").and_then(Value::as_str).unwrap_or("").to_owned() };
    Some(StageUpdate { channel_id, guild_id, topic, ended })
}

pub fn parse_effect(data: &str) -> Option<EffectUpdate> {
    let value: Value = serde_json::from_str(data).ok()?;
    let channel_id = text(&value, "channel_id")?;
    let user_id = text(&value, "user_id")?;
    let sound_id = value.get("sound_id").and_then(Value::as_str).or_else(|| value.get("sound_id").and_then(Value::as_u64).map(|_| "")).filter(|id| !id.is_empty()).map(str::to_owned)
        .or_else(|| value.get("sound_id").and_then(Value::as_u64).map(|n| n.to_string()))
        .or_else(|| value.get("sound_id").and_then(Value::as_i64).map(|n| n.to_string()))?;
    if sound_id.is_empty() {
        return None;
    }
    let guild_id = value.get("guild_id").and_then(Value::as_str).map(str::to_owned);
    let emoji = emoji_name(value.get("emoji"));
    let volume = volume_portion(value.get("sound_volume"));
    Some(EffectUpdate { channel_id, guild_id, user_id, sound_id, emoji, volume })
}

/// Settings percent times the sound's own portion. 0 plays nothing.
pub fn effect_gain(settings_percent: u8, sound_volume: f32) -> u8 {
    if settings_percent == 0 || sound_volume <= 0.0 {
        return 0;
    }
    let portion = if sound_volume <= 1.0 { sound_volume } else { (sound_volume / 100.0).min(1.0) };
    ((f32::from(settings_percent) * portion).round() as u8).min(100)
}

pub fn sound_cdn(id: &str) -> String {
    format!("https://cdn.discordapp.com/soundboard-sounds/{id}")
}

/// A file name made only of the id's safe characters, so a payload cannot choose a path.
pub fn sound_file_name(id: &str) -> String {
    let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(64).collect();
    if safe.is_empty() { "sound".to_owned() } else { safe }
}

pub fn parse_soundboard(data: &str) -> Vec<SoundboardSound> {
    let value: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let guild = value.get("guild_id").and_then(Value::as_str).map(str::to_owned);
    if value.get("soundboard_sounds").is_none() && value.get("sound_id").is_some() {
        return one_sound(&value, guild, false).into_iter().collect();
    }
    let list = value.get("soundboard_sounds").and_then(Value::as_array).cloned()
        .or_else(|| value.as_array().cloned())
        .unwrap_or_default();
    list.into_iter().filter_map(|sound| one_sound(&sound, guild.clone(), false)).collect()
}

pub fn parse_default_sounds(body: &str) -> Vec<SoundboardSound> {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let list = value.as_array().cloned().or_else(|| value.get("items").and_then(Value::as_array).cloned()).unwrap_or_default();
    list.into_iter().filter_map(|sound| one_sound(&sound, None, true)).collect()
}

pub fn upsert_sound(sounds: &mut Vec<SoundboardSound>, sound: SoundboardSound) {
    if let Some(found) = sounds.iter_mut().find(|known| known.id == sound.id) {
        *found = sound;
    } else {
        sounds.push(sound);
    }
}

pub fn delete_sound(sounds: &mut Vec<SoundboardSound>, id: &str) {
    sounds.retain(|sound| sound.id != id);
}

/// The body of `PATCH /guilds/{guild}/voice-states/@me` or `.../{user}`.
pub fn stage_body(channel_id: &str, action: StageVoiceAction, now_iso: &str) -> Value {
    match action {
        StageVoiceAction::RequestToSpeak => json!({
            "channel_id": channel_id,
            "suppress": true,
            "request_to_speak_timestamp": now_iso,
        }),
        StageVoiceAction::Accept | StageVoiceAction::Invite => json!({
            "channel_id": channel_id,
            "suppress": false,
        }),
        StageVoiceAction::Audience | StageVoiceAction::ToAudience => json!({
            "channel_id": channel_id,
            "suppress": true,
            "request_to_speak_timestamp": null,
        }),
    }
}

pub fn send_sound_body(sound_id: &str, guild_id: Option<&str>) -> Value {
    json!({ "sound_id": sound_id, "source_guild_id": guild_id })
}

fn one_sound(sound: &Value, guild: Option<String>, default: bool) -> Option<SoundboardSound> {
    let id = sound.get("sound_id").and_then(Value::as_str).filter(|id| !id.is_empty())
        .map(str::to_owned)
        .or_else(|| sound.get("sound_id").and_then(Value::as_u64).map(|n| n.to_string()))?;
    let name = sound.get("name").and_then(Value::as_str).unwrap_or("Sound").to_owned();
    let emoji = sound.get("emoji_name").and_then(Value::as_str).filter(|e| !e.is_empty()).map(str::to_owned)
        .unwrap_or_else(|| emoji_name(sound.get("emoji")));
    let guild_id = sound.get("guild_id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned).or(guild);
    let available = sound.get("available").and_then(Value::as_bool).unwrap_or(true);
    if !available {
        return None;
    }
    let volume = sound.get("volume").and_then(Value::as_f64).map(|n| if n <= 1.0 { (n * 100.0).round() as u8 } else { n.round() as u8 }).unwrap_or(100);
    Some(SoundboardSound { id, name, emoji, guild_id: guild_id.filter(|_| !default), default, volume })
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_owned)
}

fn user_ids(embedded: &Value) -> Vec<String> {
    let list = embedded.get("users").or_else(|| embedded.get("user_ids")).and_then(Value::as_array);
    list.map(|users| {
        users.iter().filter_map(|user| user.as_str().map(str::to_owned).or_else(|| user.get("id").and_then(Value::as_str).map(str::to_owned))).collect()
    }).unwrap_or_default()
}

fn emoji_name(emoji: Option<&Value>) -> String {
    let Some(emoji) = emoji else {
        return String::new();
    };
    if let Some(name) = emoji.as_str() {
        return name.to_owned();
    }
    emoji.get("name").and_then(Value::as_str).unwrap_or("").to_owned()
}

fn volume_portion(value: Option<&Value>) -> f32 {
    let Some(value) = value else {
        return 1.0;
    };
    let n = value.as_f64().or_else(|| value.as_u64().map(|n| n as f64)).unwrap_or(1.0) as f32;
    if n <= 0.0 { 0.0 } else if n <= 1.0 { n } else { (n / 100.0).min(1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_activity_names_its_application_and_an_empty_one_ends() {
        let live = parse_activity(r#"{"guild_id":"81","channel_id":"4201","embedded_activity":{"application_id":"99","users":[{"id":"7"}, "8"]}}"#).unwrap();
        assert_eq!(live.application_id.as_deref(), Some("99"));
        assert_eq!(live.users, ["7", "8"]);
        let ended = parse_activity(r#"{"channel_id":"4201","embedded_activity":null}"#).unwrap();
        assert!(ended.application_id.is_none());
        let v2 = parse_activity(r#"{"channel_id":"4201","embedded_activities":[{"application_id":"100","user_ids":["3"]}]}"#).unwrap();
        assert_eq!(v2.users, ["3"]);
    }

    #[test]
    fn applications_are_read_by_id_and_the_icon_is_the_cdn() {
        let apps = parse_applications(r#"[{"id":"99","name":"Poker","icon":"abc"}]"#);
        assert_eq!(apps, vec![("99".into(), "Poker".into(), Some("abc".into()))]);
        assert_eq!(app_icon_url("99", "abc"), "https://cdn.discordapp.com/app-icons/99/abc.png");
    }

    #[test]
    fn a_stage_topic_arrives_and_a_delete_clears_it() {
        let live = parse_stage(r#"{"id":"1","guild_id":"81","channel_id":"4202","topic":"Town hall"}"#, false).unwrap();
        assert_eq!(live.topic, "Town hall");
        let gone = parse_stage(r#"{"channel_id":"4202","topic":"Town hall"}"#, true).unwrap();
        assert!(gone.ended && gone.topic.is_empty());
    }

    #[test]
    fn a_sound_effect_scales_its_volume_and_zero_is_silent() {
        let effect = parse_effect(r#"{"channel_id":"4201","user_id":"7","sound_id":"55","sound_volume":0.5,"emoji":{"name":"🎉"}}"#).unwrap();
        assert_eq!(effect.emoji, "🎉");
        assert_eq!(effect_gain(80, effect.volume), 40);
        assert_eq!(effect_gain(0, 1.0), 0);
        assert_eq!(effect_gain(50, 0.0), 0);
        assert_eq!(sound_file_name("../etc/passwd"), "etcpasswd");
    }

    #[test]
    fn soundboard_lists_and_stage_bodies_match_discord() {
        let sounds = parse_soundboard(r#"{"guild_id":"81","soundboard_sounds":[{"name":"Horn","sound_id":"55","volume":0.8,"emoji_name":"📯","available":true}]}"#);
        assert_eq!(sounds[0].name, "Horn");
        assert_eq!(sounds[0].volume, 80);
        assert!(!sounds[0].default);
        let defaults = parse_default_sounds(r#"[{"name":"Quack","sound_id":"1","emoji_name":"🦆"}]"#);
        assert!(defaults[0].default && defaults[0].guild_id.is_none());
        let raised = stage_body("4202", StageVoiceAction::RequestToSpeak, "2026-11-22T00:00:00Z");
        assert_eq!(raised["suppress"], true);
        assert_eq!(raised["request_to_speak_timestamp"], "2026-11-22T00:00:00Z");
        let invite = stage_body("4202", StageVoiceAction::Invite, "");
        assert_eq!(invite["suppress"], false);
        let audience = stage_body("4202", StageVoiceAction::Audience, "");
        assert!(audience["request_to_speak_timestamp"].is_null());
        assert_eq!(send_sound_body("55", Some("81"))["source_guild_id"], "81");
    }
}
