//! Account settings are requested by Settings and never persisted by the adapter.

use super::{info::encode_audit_reason, model::iso_seconds, refusal, Discord, Refusal, Refused};
use protocol::{ActiveSession, DiscordAccountEntry, DiscordAccountEntryId, DiscordAccountList};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

const SESSIONS: &str = "/auth/sessions";
const AUTHORIZED_APPS: &str = "/oauth2/tokens";
const CONNECTIONS: &str = "/users/@me/connections";

#[derive(Deserialize)]
struct Sessions {
    user_sessions: Vec<Session>,
}

#[derive(Deserialize)]
struct Session {
    id_hash: String,
    approx_last_used_time: Option<String>,
    client_info: Option<SessionClient>,
}

#[derive(Default, Deserialize)]
struct SessionClient {
    platform: Option<String>,
    os: Option<String>,
    location: Option<String>,
}

#[derive(Deserialize)]
struct AuthorizedApp {
    id: String,
    application: Application,
}

#[derive(Deserialize)]
struct Application {
    name: String,
}

#[derive(Deserialize)]
struct Connection {
    id: String,
    #[serde(rename = "type")]
    provider: String,
    name: String,
}

fn session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=_-".contains(&byte))
}

fn segment(raw: &str) -> Result<String, Refusal> {
    if raw.is_empty() || raw.len() > 512 || matches!(raw, "." | "..") {
        return Err(Refusal::Refused(
            "Discord returned an invalid account entry.".to_owned(),
        ));
    }
    Ok(encode_audit_reason(raw))
}

fn account_refusal(error: Refused) -> Refusal {
    match error {
        Refused::Status {
            code: 60003 | 60008,
            ..
        }
        | Refused::Challenge => Refusal::Refused(
            "Discord requires verification for this action. Complete it in Discord.".to_owned(),
        ),
        other => refusal(other),
    }
}

impl Discord {
    fn account_list<T: DeserializeOwned>(&self, path: &str) -> Result<T, Refusal> {
        let text = self.call_once("GET", path, None).map_err(account_refusal)?;
        serde_json::from_str(&text).map_err(|_| {
            Refusal::Refused("Discord returned an unreadable account list.".to_owned())
        })
    }

    pub(super) fn active_sessions(&self) -> Result<Vec<ActiveSession>, Refusal> {
        let Sessions { user_sessions } = self.account_list(SESSIONS)?;
        let current = self.state.lock().unwrap().auth_session_id_hash.clone();
        let mut sessions: Vec<_> = user_sessions
            .into_iter()
            .filter(|session| session_id(&session.id_hash))
            .map(|session| {
                let client = session.client_info.unwrap_or_default();
                let is_current = current.as_deref() == Some(session.id_hash.as_str());
                ActiveSession {
                    id: session.id_hash,
                    app: client.platform.unwrap_or_default(),
                    device: String::new(),
                    platform: client.os.unwrap_or_default(),
                    location: client.location.unwrap_or_default(),
                    last_active: session
                        .approx_last_used_time
                        .as_deref()
                        .and_then(iso_seconds)
                        .unwrap_or_default(),
                    current: is_current,
                }
            })
            .collect();
        sessions.sort_by_key(|session| (!session.current, std::cmp::Reverse(session.last_active)));
        Ok(sessions)
    }

    pub(super) fn end_session(&self, id: &str) -> Result<(), Refusal> {
        if !session_id(id) {
            return Err(Refusal::Refused(
                "Discord returned an invalid device identifier.".to_owned(),
            ));
        }
        self.logout_sessions(vec![id.to_owned()])
    }

    pub(super) fn end_other_sessions(&self) -> Result<(), Refusal> {
        let sessions = self.active_sessions()?;
        self.logout_sessions(
            sessions
                .into_iter()
                .filter(|session| !session.current)
                .map(|session| session.id)
                .collect(),
        )
    }

    fn logout_sessions(&self, mut ids: Vec<String>) -> Result<(), Refusal> {
        let current = self
            .state
            .lock()
            .unwrap()
            .auth_session_id_hash
            .clone()
            .filter(|id| session_id(id))
            .ok_or_else(|| {
                Refusal::Refused(
                    "Discord did not identify this device. Manage devices in Discord.".to_owned(),
                )
            })?;
        if ids.iter().any(|id| *id == current) {
            return Err(Refusal::Refused(
                "Use Sign out to end this device's session.".to_owned(),
            ));
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > 64 {
            return Err(Refusal::Refused(
                "Discord returned too many devices. Manage devices in Discord.".to_owned(),
            ));
        }
        if ids.is_empty() {
            return Ok(());
        }
        self.call_once(
            "POST",
            "/auth/sessions/logout",
            Some(json!({"session_id_hashes": ids})),
        )
        .map(drop)
        .map_err(account_refusal)
    }

    pub(super) fn account_entries(
        &self,
        list: DiscordAccountList,
    ) -> Result<Vec<DiscordAccountEntry>, Refusal> {
        match list {
            DiscordAccountList::AuthorizedApps => {
                let apps: Vec<AuthorizedApp> = self.account_list(AUTHORIZED_APPS)?;
                Ok(apps
                    .into_iter()
                    .map(|app| DiscordAccountEntry {
                        target: DiscordAccountEntryId::AuthorizedApp { id: app.id },
                        name: app.application.name,
                    })
                    .collect())
            }
            DiscordAccountList::Connections => {
                let connections: Vec<Connection> = self.account_list(CONNECTIONS)?;
                Ok(connections
                    .into_iter()
                    .map(|connection| DiscordAccountEntry {
                        target: DiscordAccountEntryId::Connection {
                            provider: connection.provider,
                            id: connection.id,
                        },
                        name: connection.name,
                    })
                    .collect())
            }
        }
    }

    pub(super) fn remove_account_entry(
        &self,
        target: DiscordAccountEntryId,
    ) -> Result<(), Refusal> {
        let path = match target {
            DiscordAccountEntryId::AuthorizedApp { id } => {
                format!("{AUTHORIZED_APPS}/{}", segment(&id)?)
            }
            DiscordAccountEntryId::Connection { provider, id } => {
                format!("{CONNECTIONS}/{}/{}", segment(&provider)?, segment(&id)?)
            }
        };
        self.call_once("DELETE", &path, None)
            .map(drop)
            .map_err(account_refusal)
    }
}
