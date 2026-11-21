//! Discord server management: webhooks, followed channels, bots and integrations (#407).
//!
//! Webhook URLs contain secret tokens (`https://discord.com/api/webhooks/{id}/{token}`).
//! Never log tokens or full webhook URLs.

use super::super::Refusal;
use super::model::{self, safe_discord_id, MANAGE_GUILD, MANAGE_WEBHOOKS};
use super::{refusal, Discord};
use protocol::{MediaFile, SpaceIntegration, SpaceIntegrations, SpaceWebhook, WebhookKind};
use serde::Deserialize;
use serde_json::{json, Map, Value};

#[derive(Deserialize, Default)]
struct RawWebhookUser {
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSourceGuild {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct RawSourceChannel {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawWebhook {
    id: String,
    #[serde(rename = "type", default)]
    kind: u8,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    user: Option<RawWebhookUser>,
    #[serde(default)]
    source_guild: Option<RawSourceGuild>,
    #[serde(default)]
    source_channel: Option<RawSourceChannel>,
}

#[derive(Deserialize, Default)]
struct RawIntegrationAccount {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct RawIntegrationBot {
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
    #[serde(default)]
    discriminator: String,
}

#[derive(Deserialize, Default)]
struct RawIntegrationApp {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    bot: Option<RawIntegrationBot>,
}

#[derive(Deserialize)]
struct RawIntegration {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    account: Option<RawIntegrationAccount>,
    #[serde(default)]
    application: Option<RawIntegrationApp>,
}

fn map_webhook(
    raw: RawWebhook,
    channel_name: impl Fn(&str) -> String,
    media: &mut Vec<(MediaFile, String)>,
) -> SpaceWebhook {
    let channel_id = raw.channel_id.unwrap_or_default();
    let ch_name = if channel_id.is_empty() { String::new() } else { channel_name(&channel_id) };
    let kind = match raw.kind {
        2 => WebhookKind::ChannelFollower,
        3 => WebhookKind::Application,
        _ => WebhookKind::Incoming,
    };
    let url = match (kind, raw.token.as_deref()) {
        (WebhookKind::Incoming, Some(token)) if !token.is_empty() && safe_discord_id(&raw.id) => {
            format!("https://discord.com/api/webhooks/{}/{token}", raw.id)
        }
        _ => String::new(),
    };
    let avatar = raw.avatar.as_deref().filter(|h| !h.is_empty()).map(|hash| {
        let pair = model::avatar(&raw.id, Some(hash), "0");
        let file = pair.0.clone();
        media.push(pair);
        file
    });
    let creator = raw
        .user
        .map(|u| u.global_name.filter(|s| !s.is_empty()).unwrap_or(u.username))
        .unwrap_or_default();
    SpaceWebhook {
        id: raw.id,
        name: raw.name.unwrap_or_default(),
        channel_id,
        channel_name: ch_name,
        avatar,
        kind,
        url,
        creator,
        source_guild: raw.source_guild.map(|g| g.name).unwrap_or_default(),
        source_channel: raw.source_channel.map(|c| c.name).unwrap_or_default(),
    }
}

fn map_integration(raw: RawIntegration, media: &mut Vec<(MediaFile, String)>) -> SpaceIntegration {
    let app = raw.application.unwrap_or_default();
    let bot = app.bot.unwrap_or_default();
    let bot_name = bot.global_name.filter(|s| !s.is_empty()).unwrap_or(bot.username);
    let avatar = if !bot.id.is_empty() {
        let pair = model::avatar(&bot.id, bot.avatar.as_deref(), &bot.discriminator);
        let file = pair.0.clone();
        media.push(pair);
        Some(file)
    } else {
        None
    };
    let name = if !raw.name.is_empty() { raw.name } else { app.name };
    SpaceIntegration {
        id: raw.id,
        name,
        kind: raw.kind,
        enabled: raw.enabled.unwrap_or(true),
        application_id: app.id,
        bot_id: bot.id,
        bot_name,
        account_name: raw.account.map(|a| a.name).unwrap_or_default(),
        description: app.description,
        avatar,
    }
}

fn validate_webhook_name(raw: &str) -> Result<String, Refusal> {
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > 80 {
        return Err(Refusal::Refused("Webhook name must be between 1 and 80 characters.".to_owned()));
    }
    Ok(name.to_owned())
}

impl Discord {
    fn guild_manage_perms(&self, guild_id: &str) -> Result<(bool, bool), Refusal> {
        if !safe_discord_id(guild_id) {
            return Err(Refusal::Refused("Invalid server id.".to_owned()));
        }
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild_id) else {
            return Err(Refusal::Refused("This server is not available.".to_owned()));
        };
        let perms = info.base_permissions(guild_id);
        Ok((perms & MANAGE_WEBHOOKS != 0, perms & MANAGE_GUILD != 0))
    }

    fn channel_name_for(&self, channel_id: &str) -> String {
        self.ctx
            .known(channel_id)
            .ok()
            .flatten()
            .map(|c| c.title)
            .unwrap_or_default()
    }

    /// Reads webhooks, followed channels, and installed bots/integrations for `guild_id`.
    pub(super) fn space_integrations(&self, guild_id: &str) -> Result<SpaceIntegrations, Refusal> {
        let (can_webhooks, can_guild) = self.guild_manage_perms(guild_id)?;
        if !can_webhooks && !can_guild {
            return Err(Refusal::Refused(
                "You need Manage Webhooks or Manage Server permission to view integrations.".to_owned(),
            ));
        }
        let mut media = Vec::new();
        let mut webhooks = Vec::new();
        let mut followers = Vec::new();
        if can_webhooks {
            let body = self
                .call("GET", &format!("/guilds/{guild_id}/webhooks"), None)
                .map_err(refusal)?;
            let raw: Vec<RawWebhook> = serde_json::from_str(&body)
                .map_err(|err| Refusal::Refused(format!("Discord's webhook list was unreadable: {err}")))?;
            for item in raw {
                let mapped = map_webhook(item, |id| self.channel_name_for(id), &mut media);
                if mapped.kind == WebhookKind::ChannelFollower {
                    followers.push(mapped);
                } else {
                    webhooks.push(mapped);
                }
            }
        }
        let mut integrations = Vec::new();
        if can_guild {
            let body = self
                .call("GET", &format!("/guilds/{guild_id}/integrations?include_applications=true"), None)
                .map_err(refusal)?;
            let raw: Vec<RawIntegration> = serde_json::from_str(&body)
                .map_err(|err| Refusal::Refused(format!("Discord's integrations list was unreadable: {err}")))?;
            for item in raw {
                integrations.push(map_integration(item, &mut media));
            }
        }
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        Ok(SpaceIntegrations {
            webhooks,
            followers,
            integrations,
            can_manage_webhooks: can_webhooks,
            can_manage_guild: can_guild,
            add_bot_url: "https://discord.com/discovery/applications".to_owned(),
            permissions_url: format!("https://discord.com/channels/{guild_id}"),
        })
    }

    /// Creates an incoming webhook in `channel_id` (`POST /channels/{id}/webhooks`).
    pub(super) fn create_webhook(&self, channel_id: &str, name: &str) -> Result<SpaceWebhook, Refusal> {
        if !safe_discord_id(channel_id) {
            return Err(Refusal::Refused("Invalid channel id.".to_owned()));
        }
        let name = validate_webhook_name(name)?;
        let body = self
            .call("POST", &format!("/channels/{channel_id}/webhooks"), Some(json!({ "name": name })))
            .map_err(refusal)?;
        let raw: RawWebhook = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's webhook answer was unreadable: {err}")))?;
        let mut media = Vec::new();
        let mapped = map_webhook(raw, |id| self.channel_name_for(id), &mut media);
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        Ok(mapped)
    }

    /// Renames or moves an existing webhook (`PATCH /webhooks/{id}`).
    pub(super) fn edit_webhook(
        &self,
        guild_id: &str,
        webhook_id: &str,
        name: Option<String>,
        channel_id: Option<String>,
    ) -> Result<SpaceWebhook, Refusal> {
        let (can_webhooks, _) = self.guild_manage_perms(guild_id)?;
        if !can_webhooks {
            return Err(Refusal::Refused("You need Manage Webhooks permission to edit webhooks.".to_owned()));
        }
        if !safe_discord_id(webhook_id) {
            return Err(Refusal::Refused("Invalid webhook id.".to_owned()));
        }
        let mut patch = Map::new();
        if let Some(name) = name {
            patch.insert("name".to_owned(), Value::String(validate_webhook_name(&name)?));
        }
        if let Some(ch) = channel_id {
            if !safe_discord_id(&ch) {
                return Err(Refusal::Refused("Invalid target channel id.".to_owned()));
            }
            patch.insert("channel_id".to_owned(), Value::String(ch));
        }
        if patch.is_empty() {
            return Err(Refusal::Refused("Nothing to change on this webhook.".to_owned()));
        }
        let body = self
            .call("PATCH", &format!("/webhooks/{webhook_id}"), Some(Value::Object(patch)))
            .map_err(refusal)?;
        let raw: RawWebhook = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's webhook answer was unreadable: {err}")))?;
        let mut media = Vec::new();
        let mapped = map_webhook(raw, |id| self.channel_name_for(id), &mut media);
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        Ok(mapped)
    }

    /// Deletes a webhook or unfollows a channel-follower webhook (`DELETE /webhooks/{id}`).
    pub(super) fn delete_webhook(&self, guild_id: &str, webhook_id: &str) -> Result<(), Refusal> {
        let (can_webhooks, _) = self.guild_manage_perms(guild_id)?;
        if !can_webhooks {
            return Err(Refusal::Refused("You need Manage Webhooks permission to delete webhooks.".to_owned()));
        }
        if !safe_discord_id(webhook_id) {
            return Err(Refusal::Refused("Invalid webhook id.".to_owned()));
        }
        self.call("DELETE", &format!("/webhooks/{webhook_id}"), None)
            .map(drop)
            .map_err(refusal)
    }

    /// Removes an installed bot or integration (`DELETE /guilds/{guild}/integrations/{id}`).
    pub(super) fn remove_integration(&self, guild_id: &str, integration_id: &str) -> Result<(), Refusal> {
        let (_, can_guild) = self.guild_manage_perms(guild_id)?;
        if !can_guild {
            return Err(Refusal::Refused("You need Manage Server permission to remove integrations.".to_owned()));
        }
        if !safe_discord_id(integration_id) {
            return Err(Refusal::Refused("Invalid integration id.".to_owned()));
        }
        self.call("DELETE", &format!("/guilds/{guild_id}/integrations/{integration_id}"), None)
            .map(drop)
            .map_err(refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_incoming_and_channel_follower_webhooks() {
        let raw_incoming: RawWebhook = serde_json::from_value(json!({
            "id": "9001",
            "type": 1,
            "channel_id": "4101",
            "name": "Deploy Bot",
            "avatar": null,
            "token": "tok_secret_value",
            "user": {"username": "justin", "global_name": "Justin"}
        }))
        .unwrap();
        let mut media = Vec::new();
        let hook = map_webhook(raw_incoming, |_| "general".to_owned(), &mut media);
        assert_eq!(hook.kind, WebhookKind::Incoming);
        assert_eq!(hook.channel_name, "general");
        assert_eq!(hook.creator, "Justin");
        assert_eq!(hook.url, "https://discord.com/api/webhooks/9001/tok_secret_value");

        let raw_follower: RawWebhook = serde_json::from_value(json!({
            "id": "9002",
            "type": 2,
            "channel_id": "4102",
            "name": "Rust #announcements",
            "token": "ignored_for_follower",
            "source_guild": {"name": "Rust"},
            "source_channel": {"name": "announcements"}
        }))
        .unwrap();
        let follower = map_webhook(raw_follower, |_| "news".to_owned(), &mut media);
        assert_eq!(follower.kind, WebhookKind::ChannelFollower);
        assert!(follower.url.is_empty(), "channel followers do not expose incoming webhook URLs");
        assert_eq!(follower.source_guild, "Rust");
        assert_eq!(follower.source_channel, "announcements");
    }

    #[test]
    fn maps_bot_integration_with_application_metadata() {
        let raw: RawIntegration = serde_json::from_value(json!({
            "id": "8001",
            "name": "GitHub",
            "type": "discord",
            "enabled": true,
            "account": {"name": "GitHub"},
            "application": {
                "id": "7001",
                "name": "GitHub App",
                "description": "Repo notifications",
                "bot": {"id": "7002", "username": "github", "global_name": "GitHub Bot", "avatar": "abc", "discriminator": "0"}
            }
        }))
        .unwrap();
        let mut media = Vec::new();
        let item = map_integration(raw, &mut media);
        assert_eq!(item.id, "8001");
        assert_eq!(item.kind, "discord");
        assert_eq!(item.application_id, "7001");
        assert_eq!(item.bot_id, "7002");
        assert_eq!(item.bot_name, "GitHub Bot");
        assert!(item.avatar.is_some());
        assert_eq!(media.len(), 1);
    }

    #[test]
    fn validates_webhook_names_and_discord_ids() {
        assert!(validate_webhook_name("").is_err());
        assert!(validate_webhook_name("   ").is_err());
        assert!(validate_webhook_name(&"a".repeat(81)).is_err());
        assert_eq!(validate_webhook_name("  CI Hook  ").unwrap(), "CI Hook");
        assert!(safe_discord_id("1234567890"));
        assert!(!safe_discord_id(""));
        assert!(!safe_discord_id("123/456"));
        assert!(!safe_discord_id("../admin"));
    }
}
