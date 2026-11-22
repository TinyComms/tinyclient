//! Discord interactions: message component clicks (buttons & selects), modal form opens &
//! submissions, and gateway settlement events (`INTERACTION_CREATE`, `INTERACTION_SUCCESS`,
//! `INTERACTION_FAILURE`, `INTERACTION_MODAL_CREATE`).

use super::super::Refusal;
use super::model::Component;
use super::{refusal, Discord};
use protocol::{InteractionModal, ModalField, MODAL_FIELDS_LIMIT};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum in-flight interaction nonces tracked per session.
const MAX_PENDING_INTERACTIONS: usize = 64;
/// Discord's snowflake epoch (2015-01-01T00:00:00Z) in milliseconds.
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

static NONCE_SEQ: AtomicU64 = AtomicU64::new(1);

/// One interaction sent to `POST /interactions` while waiting for gateway settlement.
#[derive(Clone, Debug)]
pub(super) struct PendingInteraction {
    pub conversation_id: String,
    pub message_id: String,
    pub action_id: String,
}

#[derive(Deserialize)]
pub(super) struct InteractionOutcome {
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub reason_code: Option<u32>,
}

#[derive(Deserialize)]
struct ModalApp {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
pub(super) struct ModalCreate {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub custom_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub application: Option<ModalApp>,
    #[serde(default)]
    pub components: Vec<Component>,
}

/// Generates a Discord-shaped numeric snowflake string for interaction `nonce`.
pub(super) fn snowflake_nonce() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(DISCORD_EPOCH_MS);
    let delta = now_ms.saturating_sub(DISCORD_EPOCH_MS);
    let seq = NONCE_SEQ.fetch_add(1, Ordering::Relaxed) & 0xfff;
    ((delta << 22) | seq).to_string()
}

/// Parses `"discord:<app_id>:<flags>"` carried on `MessageControl::block_id`.
pub(super) fn parse_block_meta(block_id: &str) -> (Option<&str>, u64) {
    let Some(rest) = block_id.strip_prefix("discord:") else {
        return (None, 0);
    };
    match rest.split_once(':') {
        Some((app, flags)) => {
            let app = (!app.is_empty()).then_some(app);
            let flags = flags.parse::<u64>().unwrap_or(0);
            (app, flags)
        }
        None => ((!rest.is_empty()).then_some(rest), 0),
    }
}

/// Extracts `ModalField`s from a Discord `INTERACTION_MODAL_CREATE` component tree.
pub(super) fn extract_modal_fields(components: &[Component]) -> Vec<ModalField> {
    let mut fields = Vec::new();
    for row in components {
        if fields.len() >= MODAL_FIELDS_LIMIT {
            break;
        }
        let iter: &[Component] = if row.kind == 1 { &row.components } else { std::slice::from_ref(row) };
        for item in iter {
            if fields.len() >= MODAL_FIELDS_LIMIT {
                break;
            }
            if item.kind != 4 {
                continue;
            }
            let Some(custom_id) = item.custom_id.as_deref().filter(|id| !id.is_empty()) else {
                continue;
            };
            fields.push(ModalField {
                custom_id: custom_id.to_owned(),
                label: item.label.clone().unwrap_or_else(|| custom_id.to_owned()),
                value: item.value.clone().unwrap_or_default(),
                placeholder: item.placeholder.clone().unwrap_or_default(),
                required: item.required.unwrap_or(true),
                multiline: item.style.unwrap_or(1) == 2,
                min_length: item.min_length,
                max_length: item.max_length,
            });
        }
    }
    fields
}

impl Discord {
    /// Current gateway `session_id`, required by Discord's `POST /interactions`.
    pub(super) fn session_id(&self) -> Result<String, Refusal> {
        let state = self.state.lock().unwrap();
        state
            .session
            .as_ref()
            .map(|s| s.id.clone())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Refusal::Refused("Not connected to Discord.".to_owned()))
    }

    pub(super) fn remember_interaction(&self, nonce: String, pending: PendingInteraction) {
        let mut state = self.state.lock().unwrap();
        if state.pending_interactions.len() >= MAX_PENDING_INTERACTIONS {
            state.pending_interactions.clear();
        }
        state.pending_interactions.insert(nonce, pending);
    }

    pub(super) fn forget_interaction(&self, nonce: &str) -> Option<PendingInteraction> {
        self.state.lock().unwrap().pending_interactions.remove(nonce)
    }

    /// Posts a Discord message component interaction (`POST /interactions`, type 3).
    /// Never auto-retries: only fires on explicit user click/selection.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn message_action(
        &self,
        conversation_id: &str,
        message_id: &str,
        block_id: &str,
        action_id: &str,
        element_type: &str,
        value: &str,
        values: Vec<String>,
    ) -> Result<(), Refusal> {
        let session_id = self.session_id()?;
        let known = self.ctx.known(conversation_id).ok().flatten();
        let guild_id = known.as_ref().and_then(|c| self.guild_of(c));
        let (parsed_app, message_flags) = parse_block_meta(block_id);
        let application_id = match parsed_app {
            Some(app) => app.to_owned(),
            None => self
                .ctx
                .stored_message(conversation_id, message_id)
                .ok()
                .flatten()
                .map(|m| m.sender.id)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| Refusal::Refused("Could not determine which application owns this control.".to_owned()))?,
        };
        let component_type: u8 = match element_type {
            "discord_string_select" | "static_select" => 3,
            "discord_user_select" => 5,
            "discord_role_select" => 6,
            "discord_mentionable_select" => 7,
            "discord_channel_select" => 8,
            _ => 2,
        };
        let mut data = json!({
            "component_type": component_type,
            "custom_id": action_id,
        });
        if component_type != 2 {
            let chosen = if !values.is_empty() {
                values
            } else if !value.is_empty() {
                vec![value.to_owned()]
            } else {
                Vec::new()
            };
            data["values"] = json!(chosen);
        }
        let nonce = snowflake_nonce();
        let mut body = json!({
            "type": 3,
            "nonce": nonce,
            "channel_id": conversation_id,
            "message_flags": message_flags,
            "message_id": message_id,
            "application_id": application_id,
            "session_id": session_id,
            "data": data,
        });
        if let Some(guild) = guild_id {
            body["guild_id"] = json!(guild);
        }
        self.remember_interaction(
            nonce.clone(),
            PendingInteraction {
                conversation_id: conversation_id.to_owned(),
                message_id: message_id.to_owned(),
                action_id: action_id.to_owned(),
            },
        );
        // Interactions must never auto-retry.
        if let Err(err) = self.rest.call_once_with("POST", "/interactions", Some(body), &[]) {
            self.forget_interaction(&nonce);
            return Err(refusal(err));
        }
        Ok(())
    }

    /// Submits a Discord interaction modal (`POST /interactions`, type 5).
    pub(super) fn modal_submit(
        &self,
        conversation_id: &str,
        modal_id: &str,
        application_id: &str,
        custom_id: &str,
        values: Vec<(String, String)>,
    ) -> Result<(), Refusal> {
        let session_id = self.session_id()?;
        let known = self.ctx.known(conversation_id).ok().flatten();
        let guild_id = known.as_ref().and_then(|c| self.guild_of(c));
        let rows: Vec<Value> = values
            .into_iter()
            .take(MODAL_FIELDS_LIMIT)
            .map(|(field_id, val)| {
                json!({
                    "type": 1,
                    "components": [{
                        "type": 4,
                        "custom_id": field_id,
                        "value": val,
                    }]
                })
            })
            .collect();
        let nonce = snowflake_nonce();
        let mut body = json!({
            "type": 5,
            "nonce": nonce,
            "application_id": application_id,
            "channel_id": conversation_id,
            "session_id": session_id,
            "data": {
                "id": modal_id,
                "custom_id": custom_id,
                "components": rows,
            },
        });
        if let Some(guild) = guild_id {
            body["guild_id"] = json!(guild);
        }
        self.remember_interaction(
            nonce.clone(),
            PendingInteraction {
                conversation_id: conversation_id.to_owned(),
                message_id: String::new(),
                action_id: custom_id.to_owned(),
            },
        );
        if let Err(err) = self.rest.call_once_with("POST", "/interactions", Some(body), &[]) {
            self.forget_interaction(&nonce);
            return Err(refusal(err));
        }
        Ok(())
    }

    /// Gateway `INTERACTION_CREATE` / `INTERACTION_SUCCESS`: clears pending state for `nonce`.
    pub(super) fn interaction_settled(&self, outcome: InteractionOutcome) {
        if let Some(nonce) = outcome.nonce.as_deref() {
            self.forget_interaction(nonce);
        }
    }

    /// Gateway `INTERACTION_FAILURE`: notifies the shell so pending buttons stop spinning and show the failure.
    pub(super) fn interaction_failed(&self, outcome: InteractionOutcome) {
        let Some(nonce) = outcome.nonce.as_deref() else {
            return;
        };
        let Some(pending) = self.forget_interaction(nonce) else {
            return;
        };
        let reason = match outcome.reason_code {
            Some(1) => "The application did not respond in time.".to_owned(),
            Some(2) => "This interaction has expired.".to_owned(),
            _ => "This interaction failed.".to_owned(),
        };
        self.ctx
            .interaction_failed(&pending.conversation_id, &pending.message_id, &pending.action_id, &reason);
    }

    /// Gateway `INTERACTION_MODAL_CREATE`: delivers the form to the shell for the active conversation.
    pub(super) fn interaction_modal_create(&self, raw: ModalCreate) {
        let pending = raw.nonce.as_deref().and_then(|n| self.forget_interaction(n));
        let channel_id = raw
            .channel_id
            .filter(|id| !id.is_empty())
            .or_else(|| pending.map(|p| p.conversation_id));
        let Some(channel_id) = channel_id else {
            return;
        };
        let modal = InteractionModal {
            id: raw.id,
            application_id: raw.application.map(|a| a.id).unwrap_or_default(),
            custom_id: raw.custom_id,
            title: raw.title,
            fields: extract_modal_fields(&raw.components),
            nonce: raw.nonce.unwrap_or_default(),
        };
        self.ctx.interaction_modal(&channel_id, modal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_block_meta_reads_app_id_and_flags() {
        assert_eq!(parse_block_meta("discord:9001:64"), (Some("9001"), 64));
        assert_eq!(parse_block_meta("discord::0"), (None, 0));
        assert_eq!(parse_block_meta("slack_block"), (None, 0));
    }

    #[test]
    fn extract_modal_fields_reads_short_and_paragraph_text_inputs() {
        let rows: Vec<Component> = serde_json::from_value(json!([
            {
                "type": 1,
                "components": [{
                    "type": 4,
                    "custom_id": "title",
                    "label": "Title",
                    "style": 1,
                    "placeholder": "Short summary",
                    "min_length": 3,
                    "max_length": 80,
                    "required": true
                }]
            },
            {
                "type": 1,
                "components": [{
                    "type": 4,
                    "custom_id": "details",
                    "label": "Details",
                    "style": 2,
                    "value": "Initial text",
                    "required": false
                }]
            }
        ]))
        .unwrap();
        let fields = extract_modal_fields(&rows);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].custom_id, "title");
        assert!(!fields[0].multiline && fields[0].required);
        assert_eq!((fields[0].min_length, fields[0].max_length), (Some(3), Some(80)));
        assert_eq!(fields[1].custom_id, "details");
        assert!(fields[1].multiline && !fields[1].required && fields[1].value == "Initial text");
    }
}
