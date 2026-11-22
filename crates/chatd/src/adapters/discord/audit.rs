//! Discord server audit log (`GET /guilds/{id}/audit-logs`) and live
//! `GUILD_AUDIT_LOG_ENTRY_CREATE` handling (#394).

use super::super::{AuditPage, Refusal};
use super::model::{seconds, Kept, User, VIEW_AUDIT_LOG};
use super::{escaped, refusal, Discord};
use protocol::{AuditChange, AuditEntry};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Deserialize, Default)]
struct AuditLogPayload {
    #[serde(default)]
    audit_log_entries: Vec<RawAuditEntry>,
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    webhooks: Vec<NamedRef>,
    #[serde(default)]
    threads: Vec<NamedRef>,
    #[serde(default)]
    auto_moderation_rules: Vec<NamedRef>,
    #[serde(default)]
    guild_scheduled_events: Vec<NamedRef>,
    #[serde(default)]
    integrations: Vec<NamedRef>,
}

#[derive(Deserialize, Default)]
struct NamedRef {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawAuditChange {
    #[serde(default)]
    key: String,
    #[serde(default)]
    old_value: Option<Value>,
    #[serde(default)]
    new_value: Option<Value>,
}

#[derive(Deserialize, Default)]
pub(super) struct RawAuditEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub action_type: u32,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub changes: Vec<RawAuditChange>,
    #[serde(default)]
    pub options: Option<Value>,
}

/// Human-readable label and verb phrase for every documented Discord audit log action type.
/// Unknown action types are NEVER dropped — they fall back to `"Action #<n>"`.
pub(super) fn action_meta(action_type: u32) -> (String, &'static str) {
    let pair: (&str, &str) = match action_type {
        1 => ("Server Updated", "updated the server"),
        10 => ("Channel Created", "created channel"),
        11 => ("Channel Updated", "updated channel"),
        12 => ("Channel Deleted", "deleted channel"),
        13 => ("Channel Overwrite Created", "added permissions on"),
        14 => ("Channel Overwrite Updated", "updated permissions on"),
        15 => ("Channel Overwrite Deleted", "removed permissions on"),
        20 => ("Member Kicked", "kicked"),
        21 => ("Members Pruned", "pruned inactive members"),
        22 => ("Member Banned", "banned"),
        23 => ("Member Unbanned", "unbanned"),
        24 => ("Member Updated", "updated member"),
        25 => ("Member Roles Updated", "updated roles for"),
        26 => ("Member Moved", "moved"),
        27 => ("Member Disconnected", "disconnected"),
        28 => ("Bot Added", "added bot"),
        30 => ("Role Created", "created role"),
        31 => ("Role Updated", "updated role"),
        32 => ("Role Deleted", "deleted role"),
        40 => ("Invite Created", "created invite"),
        41 => ("Invite Updated", "updated invite"),
        42 => ("Invite Deleted", "deleted invite"),
        50 => ("Webhook Created", "created webhook"),
        51 => ("Webhook Updated", "updated webhook"),
        52 => ("Webhook Deleted", "deleted webhook"),
        60 => ("Emoji Created", "created emoji"),
        61 => ("Emoji Updated", "updated emoji"),
        62 => ("Emoji Deleted", "deleted emoji"),
        72 => ("Message Deleted", "deleted a message by"),
        73 => ("Messages Bulk Deleted", "bulk-deleted messages in"),
        74 => ("Message Pinned", "pinned a message by"),
        75 => ("Message Unpinned", "unpinned a message by"),
        80 => ("Integration Created", "added integration"),
        81 => ("Integration Updated", "updated integration"),
        82 => ("Integration Deleted", "removed integration"),
        83 => ("Stage Started", "started stage"),
        84 => ("Stage Updated", "updated stage"),
        85 => ("Stage Ended", "ended stage"),
        90 => ("Sticker Created", "created sticker"),
        91 => ("Sticker Updated", "updated sticker"),
        92 => ("Sticker Deleted", "deleted sticker"),
        100 => ("Event Created", "created event"),
        101 => ("Event Updated", "updated event"),
        102 => ("Event Deleted", "deleted event"),
        110 => ("Thread Created", "created thread"),
        111 => ("Thread Updated", "updated thread"),
        112 => ("Thread Deleted", "deleted thread"),
        121 => ("Command Permissions Updated", "updated command permissions for"),
        130 => ("Soundboard Sound Created", "created sound"),
        131 => ("Soundboard Sound Updated", "updated sound"),
        132 => ("Soundboard Sound Deleted", "deleted sound"),
        140 => ("AutoMod Rule Created", "created AutoMod rule"),
        141 => ("AutoMod Rule Updated", "updated AutoMod rule"),
        142 => ("AutoMod Rule Deleted", "deleted AutoMod rule"),
        143 => ("AutoMod Blocked Message", "blocked a message from"),
        144 => ("AutoMod Flagged Message", "flagged a message from"),
        145 => ("AutoMod Timed Out Member", "timed out"),
        146 => ("AutoMod Quarantined Member", "quarantined"),
        150 => ("Monetization Requested", "requested server monetization"),
        151 => ("Monetization Terms Accepted", "accepted monetization terms"),
        163 => ("Onboarding Prompt Created", "created onboarding prompt"),
        164 => ("Onboarding Prompt Updated", "updated onboarding prompt"),
        165 => ("Onboarding Prompt Deleted", "deleted onboarding prompt"),
        166 => ("Onboarding Created", "configured onboarding"),
        167 => ("Onboarding Updated", "updated onboarding"),
        190 => ("Server Guide Created", "created server guide"),
        191 => ("Server Guide Updated", "updated server guide"),
        192 => ("Voice Status Updated", "set voice status on"),
        193 => ("Voice Status Cleared", "cleared voice status on"),
        200 => ("Server Profile Updated", "updated server profile"),
        211 => ("Guild Incident Actions Updated", "updated raid protection"),
        _ => return (format!("Action #{action_type}"), "performed an action on"),
    };
    (pair.0.to_owned(), pair.1)
}

/// Formats one audit log change value into a compact human-readable string.
pub(super) fn format_change_value(value: &Value) -> String {
    match value {
        Value::Null => "none".to_owned(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_owned(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|item| {
                    item.get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format_change_value(item))
                })
                .collect();
            parts.join(", ")
        }
        Value::Object(map) => {
            if let Some(name) = map.get("name").and_then(Value::as_str) {
                name.to_owned()
            } else {
                serde_json::to_string(value).unwrap_or_default()
            }
        }
    }
}

/// Builds the REST path for `GET /guilds/{guild}/audit-logs` with optional filters & pagination.
pub(super) fn audit_log_path(
    guild: &str,
    person: Option<&str>,
    action: Option<u32>,
    before: Option<&str>,
    limit: u32,
) -> String {
    let limit = if limit == 0 { 50 } else { limit.clamp(1, 100) };
    let mut path = format!("/guilds/{guild}/audit-logs?limit={limit}");
    if let Some(uid) = person.map(str::trim).filter(|s| !s.is_empty()) {
        path.push_str(&format!("&user_id={}", escaped(uid)));
    }
    if let Some(act) = action {
        path.push_str(&format!("&action_type={act}"));
    }
    if let Some(cursor) = before.map(str::trim).filter(|s| !s.is_empty()) {
        path.push_str(&format!("&before={}", escaped(cursor)));
    }
    path
}

fn parse_audit_payload(
    raw: &str,
    cached_channels: &HashMap<String, String>,
    cached_roles: &HashMap<String, String>,
    cached_users: &HashMap<String, String>,
    limit: u32,
) -> Result<AuditPage, Refusal> {
    let payload: AuditLogPayload = serde_json::from_str(raw)
        .map_err(|err| Refusal::Refused(format!("Discord's audit log was unreadable: {err}")))?;
    let mut users: HashMap<String, String> = cached_users.clone();
    for u in &payload.users {
        if !u.id.is_empty() {
            users.insert(u.id.clone(), u.name().to_owned());
        }
    }
    let mut named: HashMap<String, String> = HashMap::new();
    for (id, name) in cached_channels {
        named.insert(id.clone(), format!("#{name}"));
    }
    for (id, name) in cached_roles {
        named.insert(id.clone(), name.clone());
    }
    for list in [
        &payload.webhooks,
        &payload.threads,
        &payload.auto_moderation_rules,
        &payload.guild_scheduled_events,
        &payload.integrations,
    ] {
        for item in list {
            if !item.id.is_empty() {
                if let Some(name) = item.name.as_deref().filter(|n| !n.is_empty()) {
                    named.insert(item.id.clone(), name.to_owned());
                }
            }
        }
    }

    let more = payload.audit_log_entries.len() as u32 >= limit.clamp(1, 100);
    let mut entries = Vec::with_capacity(payload.audit_log_entries.len());
    for entry in payload.audit_log_entries {
        let (action_label, verb) = action_meta(entry.action_type);
        let actor_id = entry.user_id.clone().filter(|id| !id.is_empty());
        let actor_name = actor_id
            .as_deref()
            .and_then(|id| users.get(id).cloned())
            .or_else(|| {
                if (143..=146).contains(&entry.action_type) {
                    Some("AutoMod".to_owned())
                } else {
                    actor_id.as_ref().map(|id| format!("User {id}"))
                }
            })
            .unwrap_or_else(|| "System".to_owned());

        let target_id = entry.target_id.clone().filter(|id| !id.is_empty());
        let name_from_change = entry
            .changes
            .iter()
            .find(|c| c.key == "name")
            .and_then(|c| c.new_value.as_ref().or(c.old_value.as_ref()))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let target_name = target_id
            .as_deref()
            .and_then(|id| users.get(id).or_else(|| named.get(id)).cloned())
            .or(name_from_change)
            .or_else(|| {
                entry
                    .options
                    .as_ref()
                    .and_then(|o| o.get("auto_moderation_rule_name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| target_id.clone())
            .unwrap_or_default();

        let summary = if target_name.is_empty() {
            format!("{actor_name} {verb}.")
        } else {
            format!("{actor_name} {verb} {target_name}.")
        };

        let changes = entry
            .changes
            .into_iter()
            .map(|c| AuditChange {
                key: c.key,
                old: c.old_value.as_ref().map(format_change_value),
                new: c.new_value.as_ref().map(format_change_value),
            })
            .collect();

        let time = if entry.id.is_empty() { 0 } else { seconds(&entry.id) };
        entries.push(AuditEntry {
            id: entry.id,
            action_type: entry.action_type,
            action_label,
            summary,
            actor_id,
            actor_name,
            target_id,
            target_name,
            reason: entry.reason.unwrap_or_default(),
            time,
            changes,
        });
    }

    Ok(AuditPage { entries, more })
}

impl Discord {
    /// Fetches one page of a server's audit log (`GET /guilds/{guild}/audit-logs`).
    pub(super) fn audit_log(
        &self,
        guild: &str,
        person: Option<&str>,
        action: Option<u32>,
        before: Option<&str>,
        limit: u32,
    ) -> Result<AuditPage, Refusal> {
        self.require_guild_perm(guild, VIEW_AUDIT_LOG, "view this server's audit log")?;
        let effective_limit = if limit == 0 { 50 } else { limit.clamp(1, 100) };
        let path = audit_log_path(guild, person, action, before, effective_limit);
        let raw = self.call("GET", &path, None).map_err(refusal)?;

        let mut cached_channels: HashMap<String, String> = HashMap::new();
        if let Some(kept_raw) = self.ctx.note(&format!("kept:{guild}")) {
            if let Ok(kept) = serde_json::from_str::<Vec<Kept>>(&kept_raw) {
                for ch in kept {
                    if let Some(name) = ch.name.filter(|n| !n.is_empty()) {
                        cached_channels.insert(ch.id, name);
                    }
                }
            }
        }
        let (cached_roles, cached_users) = {
            let state = crate::hub::lock(&self.state);
            let roles = state
                .guilds
                .get(guild)
                .map(|info| {
                    info.roles
                        .iter()
                        .map(|(rid, r)| (rid.to_string(), r.name.clone()))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            let mut users = state.users.clone();
            if let Some(info) = state.guilds.get(guild) {
                for (cid, (_, cname)) in &info.categories {
                    cached_channels.entry(cid.clone()).or_insert_with(|| cname.clone());
                }
            }
            users.entry(state.me.clone()).or_insert_with(|| "You".to_owned());
            (roles, users)
        };

        parse_audit_payload(&raw, &cached_channels, &cached_roles, &cached_users, effective_limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_meta_maps_known_and_preserves_unknown_action_types() {
        assert_eq!(action_meta(1).0, "Server Updated");
        assert_eq!(action_meta(22).0, "Member Banned");
        assert_eq!(action_meta(140).0, "AutoMod Rule Created");
        assert_eq!(action_meta(143).0, "AutoMod Blocked Message");
        assert_eq!(action_meta(192).0, "Voice Status Updated");
        assert_eq!(action_meta(999).0, "Action #999");
    }

    #[test]
    fn audit_log_path_builds_filters_and_clamps_limit() {
        assert_eq!(
            audit_log_path("4000", Some("2001"), Some(22), Some("13000001"), 500),
            "/guilds/4000/audit-logs?limit=100&user_id=2001&action_type=22&before=13000001"
        );
        assert_eq!(audit_log_path("4000", None, None, None, 0), "/guilds/4000/audit-logs?limit=50");
    }

    #[test]
    fn parse_audit_payload_resolves_sideloaded_names_changes_and_unknown_actions() {
        let payload = json!({
            "users": [
                {"id": "1000", "username": "justin", "global_name": "Justin"},
                {"id": "2001", "username": "mara", "global_name": "Mara Okafor"}
            ],
            "webhooks": [{"id": "7001", "name": "Deploy Hook"}],
            "auto_moderation_rules": [{"id": "8001", "name": "Block spam links"}],
            "audit_log_entries": [
                {
                    "id": "1300000000000000100",
                    "action_type": 22,
                    "user_id": "1000",
                    "target_id": "2001",
                    "reason": "Repeated spam",
                    "changes": []
                },
                {
                    "id": "1300000000000000090",
                    "action_type": 11,
                    "user_id": "1000",
                    "target_id": "4101",
                    "changes": [
                        {"key": "name", "old_value": "general", "new_value": "lobby"},
                        {"key": "$add", "new_value": [{"id": "4010", "name": "mods"}]}
                    ]
                },
                {
                    "id": "1300000000000000080",
                    "action_type": 50,
                    "user_id": "1000",
                    "target_id": "7001"
                },
                {
                    "id": "1300000000000000070",
                    "action_type": 140,
                    "user_id": "1000",
                    "target_id": "8001"
                },
                {
                    "id": "1300000000000000060",
                    "action_type": 999,
                    "user_id": "1000",
                    "target_id": "9999"
                }
            ]
        });
        let mut channels = HashMap::new();
        channels.insert("4101".into(), "lobby".into());
        let page = parse_audit_payload(&payload.to_string(), &channels, &HashMap::new(), &HashMap::new(), 5).unwrap();
        assert!(page.more);
        assert_eq!(page.entries.len(), 5, "unknown action_type 999 must never be dropped");
        assert_eq!(page.entries[0].summary, "Justin banned Mara Okafor.");
        assert_eq!(page.entries[0].reason, "Repeated spam");
        assert!(page.entries[0].time > 0);
        assert_eq!(page.entries[1].summary, "Justin updated channel #lobby.");
        assert_eq!(page.entries[1].changes.len(), 2);
        assert_eq!(page.entries[1].changes[0].old.as_deref(), Some("general"));
        assert_eq!(page.entries[1].changes[0].new.as_deref(), Some("lobby"));
        assert_eq!(page.entries[1].changes[1].new.as_deref(), Some("mods"));
        assert_eq!(page.entries[2].summary, "Justin created webhook Deploy Hook.");
        assert_eq!(page.entries[3].summary, "Justin created AutoMod rule Block spam links.");
        assert_eq!(page.entries[4].action_label, "Action #999");
        assert_eq!(page.entries[4].action_type, 999);
    }
}
