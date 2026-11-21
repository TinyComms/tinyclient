//! Discord server & channel management: creating, renaming, editing, reordering, and deleting
//! channels and categories (#410).

use super::super::Refusal;
use super::model::{self, number, Channel, Kept, ADMINISTRATOR, MANAGE_CHANNELS, VIEW_CHANNEL};
use super::{ready, refusal, Discord};
use crate::hub::now;
use protocol::{ChannelPosition, ConversationKind, Group};
use serde_json::{json, Map, Value};

/// Validates and maps a channel name (trimmed, 1..=100 chars).
fn clean_name(raw: &str, what: &str) -> Result<String, Refusal> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Refusal::Refused(format!("Enter a {what} name.")));
    }
    if trimmed.chars().count() > 100 {
        return Err(Refusal::Refused(format!("{what} name must be 100 characters or fewer.")));
    }
    Ok(trimmed.to_owned())
}

/// Maps a [`ConversationKind`] to Discord's channel `type` integer for creation.
pub(super) fn discord_channel_type(kind: ConversationKind) -> Result<u8, Refusal> {
    match kind {
        ConversationKind::Channel => Ok(model::TEXT),
        ConversationKind::Voice => Ok(model::VOICE),
        ConversationKind::Announcement => Ok(model::NEWS),
        ConversationKind::Forum => Ok(model::FORUM),
        _ => Err(Refusal::Refused("Discord cannot create a channel of that type here.".to_owned())),
    }
}

/// Builds the JSON body for `POST /guilds/{guild}/channels` when creating a channel.
pub(super) fn create_channel_body(
    guild: &str,
    creator: &str,
    name: &str,
    kind: ConversationKind,
    private: bool,
    parent: Option<&str>,
) -> Result<Value, Refusal> {
    let name = clean_name(name, "channel")?;
    let kind_num = discord_channel_type(kind)?;
    let mut body = Map::new();
    body.insert("name".into(), json!(name));
    body.insert("type".into(), json!(kind_num));
    if let Some(parent) = parent.filter(|p| !p.is_empty()) {
        body.insert("parent_id".into(), json!(parent));
    }
    if private {
        let mut overwrites = vec![json!({
            "id": guild,
            "type": 0,
            "allow": "0",
            "deny": VIEW_CHANNEL.to_string(),
        })];
        if !creator.is_empty() {
            overwrites.push(json!({
                "id": creator,
                "type": 1,
                "allow": VIEW_CHANNEL.to_string(),
                "deny": "0",
            }));
        }
        body.insert("permission_overwrites".into(), Value::Array(overwrites));
    }
    Ok(Value::Object(body))
}

/// Builds the JSON body for `POST /guilds/{guild}/channels` when creating a category (`type: 4`).
pub(super) fn create_category_body(name: &str) -> Result<Value, Refusal> {
    let name = clean_name(name, "category")?;
    Ok(json!({
        "name": name,
        "type": model::CATEGORY,
    }))
}

/// Builds the JSON body for `PATCH /channels/{id}` with only the fields being changed.
pub(super) fn edit_channel_body(
    name: Option<&str>,
    topic: Option<&str>,
    rate_limit_per_user: Option<u32>,
    nsfw: Option<bool>,
    user_limit: Option<u32>,
    parent_id: Option<Option<&str>>,
) -> Result<Value, Refusal> {
    let mut body = Map::new();
    if let Some(name) = name {
        body.insert("name".into(), json!(clean_name(name, "channel")?));
    }
    if let Some(topic) = topic {
        let trimmed = topic.trim();
        if trimmed.chars().count() > 1024 {
            return Err(Refusal::Refused("Channel topic must be 1024 characters or fewer.".to_owned()));
        }
        body.insert("topic".into(), if trimmed.is_empty() { Value::Null } else { json!(trimmed) });
    }
    if let Some(slow) = rate_limit_per_user {
        if slow > 21_600 {
            return Err(Refusal::Refused("Slow mode must be 6 hours (21600 s) or less.".to_owned()));
        }
        body.insert("rate_limit_per_user".into(), json!(slow));
    }
    if let Some(nsfw) = nsfw {
        body.insert("nsfw".into(), json!(nsfw));
    }
    if let Some(limit) = user_limit {
        if limit > 99 {
            return Err(Refusal::Refused("Voice user limit must be between 0 and 99.".to_owned()));
        }
        body.insert("user_limit".into(), json!(limit));
    }
    if let Some(parent) = parent_id {
        match parent.filter(|p| !p.is_empty()) {
            Some(pid) => body.insert("parent_id".into(), json!(pid)),
            None => body.insert("parent_id".into(), Value::Null),
        };
    }
    if body.is_empty() {
        return Err(Refusal::Refused("No channel changes specified.".to_owned()));
    }
    Ok(Value::Object(body))
}

/// Builds the JSON array body for `PATCH /guilds/{guild}/channels` bulk position update.
pub(super) fn move_channels_body(order: &[ChannelPosition]) -> Result<Value, Refusal> {
    let mut list = Vec::with_capacity(order.len());
    for item in order {
        if item.id.trim().is_empty() {
            return Err(Refusal::Refused("Channel id cannot be empty.".to_owned()));
        }
        let mut entry = Map::new();
        entry.insert("id".into(), json!(item.id));
        entry.insert("position".into(), json!(item.position));
        if let Some(parent) = &item.parent_id {
            match parent.as_deref().filter(|p| !p.is_empty()) {
                Some(pid) => entry.insert("parent_id".into(), json!(pid)),
                None => entry.insert("parent_id".into(), Value::Null),
            };
        }
        if let Some(lock) = item.lock_permissions {
            entry.insert("lock_permissions".into(), json!(lock));
        }
        list.push(Value::Object(entry));
    }
    Ok(Value::Array(list))
}

impl Discord {
    /// Resolves which guild owns a channel or category id.
    pub(super) fn guild_of_channel_or_category(&self, id: &str) -> Option<String> {
        if let Ok(Some(known)) = self.ctx.known(id) {
            if let Some(guild) = self.guild_of(&known) {
                return Some(guild);
            }
        }
        let target_num = number(id);
        let state = crate::hub::lock(&self.state);
        for (guild_id, info) in &state.guilds {
            if info.categories.contains_key(id) || info.channels.iter().any(|(cid, _)| *cid == target_num) {
                return Some(guild_id.clone());
            }
        }
        None
    }

    /// Checks a guild-level permission bit (and active timeout) before hitting Discord.
    pub(super) fn require_guild_perm(&self, guild: &str, perm: u64, what: &str) -> Result<(), Refusal> {
        let clock = now();
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild) else {
            return Err(Refusal::Refused("That server is no longer available.".to_owned()));
        };
        if let Some(until) = info.timed_out(clock) {
            return Err(Refusal::Refused(ready::timeout_reason(until, clock)));
        }
        let perms = info.guild_permissions(guild);
        if perms & (perm | ADMINISTRATOR) == 0 {
            return Err(Refusal::Refused(format!("You do not have permission to {what}.")));
        }
        Ok(())
    }

    /// Checks a channel-level (or category-level) permission bit, factoring in channel overwrites.
    pub(super) fn require_channel_perm(&self, guild: &str, channel_id: &str, perm: u64, what: &str) -> Result<(), Refusal> {
        let kept_channel = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|raw| serde_json::from_str::<Vec<Kept>>(&raw).ok())
            .and_then(|kept| kept.into_iter().find(|k| k.id == channel_id))
            .map(Kept::channel);
        let clock = now();
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild) else {
            return Err(Refusal::Refused("That server is no longer available.".to_owned()));
        };
        if let Some(until) = info.timed_out(clock) {
            return Err(Refusal::Refused(ready::timeout_reason(until, clock)));
        }
        let perms = match kept_channel.as_ref() {
            Some(ch) => info.permissions(guild, &state.me, ch),
            None => info.guild_permissions(guild),
        };
        if perms & (perm | ADMINISTRATOR) == 0 {
            return Err(Refusal::Refused(format!("You do not have permission to {what}.")));
        }
        Ok(())
    }

    /// Creates a Discord channel (`POST /guilds/{guild}/channels`).
    pub(super) fn create_channel(
        &self,
        guild: &str,
        name: &str,
        kind: ConversationKind,
        private: bool,
        group: Option<&str>,
    ) -> Result<String, Refusal> {
        self.require_guild_perm(guild, MANAGE_CHANNELS, "create channels in this server")?;
        let me = self.me();
        let body = create_channel_body(guild, &me, name, kind, private, group)?;
        let raw = self.call("POST", &format!("/guilds/{guild}/channels"), Some(body)).map_err(refusal)?;
        let mut channel: Channel = serde_json::from_str(&raw)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        if channel.guild_id.is_none() {
            channel.guild_id = Some(guild.to_owned());
        }
        let id = channel.id.clone();
        self.channel_changed(channel).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(id)
    }

    /// Creates a Discord category (`POST /guilds/{guild}/channels` with `type: 4`).
    pub(super) fn create_category(&self, guild: &str, name: &str) -> Result<Group, Refusal> {
        self.require_guild_perm(guild, MANAGE_CHANNELS, "create categories in this server")?;
        let body = create_category_body(name)?;
        let raw = self.call("POST", &format!("/guilds/{guild}/channels"), Some(body)).map_err(refusal)?;
        let mut channel: Channel = serde_json::from_str(&raw)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        if channel.guild_id.is_none() {
            channel.guild_id = Some(guild.to_owned());
        }
        let title = channel.name.clone().unwrap_or_else(|| name.trim().to_owned());
        let group = ready::header(&channel.id, &title, channel.position);
        self.keep(guild, |kept| match kept.iter_mut().find(|k| k.id == channel.id) {
            Some(existing) => *existing = Kept::of(&channel),
            None => kept.push(Kept::of(&channel)),
        });
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                info.categories.insert(channel.id.clone(), (channel.position, title));
            }
        }
        self.ctx.group(guild, &group).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(group)
    }

    /// Edits a Discord channel or category (`PATCH /channels/{id}`).
    pub(super) fn edit_channel(
        &self,
        channel_id: &str,
        name: Option<&str>,
        topic: Option<&str>,
        rate_limit_per_user: Option<u32>,
        nsfw: Option<bool>,
        user_limit: Option<u32>,
        parent_id: Option<Option<&str>>,
    ) -> Result<(), Refusal> {
        let Some(guild) = self.guild_of_channel_or_category(channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        self.require_channel_perm(&guild, channel_id, MANAGE_CHANNELS, "edit this channel")?;
        let body = edit_channel_body(name, topic, rate_limit_per_user, nsfw, user_limit, parent_id)?;
        let raw = self.call("PATCH", &format!("/channels/{channel_id}"), Some(body)).map_err(refusal)?;
        let mut channel: Channel = serde_json::from_str(&raw)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        if channel.guild_id.is_none() {
            channel.guild_id = Some(guild.clone());
        }
        let is_category = channel.kind == model::CATEGORY;
        let cat_group = is_category.then(|| {
            let title = channel.name.clone().unwrap_or_default();
            ready::header(&channel.id, &title, channel.position)
        });
        self.channel_changed(channel).map_err(|err| Refusal::Refused(err.to_string()))?;
        // Empty categories are skipped by `category_changed`; still update their header if present.
        if let Some(group) = cat_group {
            let _ = self.ctx.group(&guild, &group);
        }
        Ok(())
    }

    /// Reorders channels/categories and moves channels across categories in one bulk
    /// `PATCH /guilds/{guild}/channels` request.
    pub(super) fn move_channels(&self, guild: &str, order: &[ChannelPosition]) -> Result<(), Refusal> {
        self.require_guild_perm(guild, MANAGE_CHANNELS, "reorder channels in this server")?;
        if order.is_empty() {
            return Ok(());
        }
        let body = move_channels_body(order)?;
        self.call("PATCH", &format!("/guilds/{guild}/channels"), Some(body)).map_err(refusal)?;
        self.keep(guild, |kept| {
            for item in order {
                if let Some(found) = kept.iter_mut().find(|k| k.id == item.id) {
                    found.position = item.position;
                    if let Some(parent) = &item.parent_id {
                        found.parent = parent.clone().filter(|p| !p.is_empty());
                    }
                }
            }
        });
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                for item in order {
                    if let Some((pos, _)) = info.categories.get_mut(&item.id) {
                        *pos = item.position;
                    }
                }
            }
        }
        self.replant(guild).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(())
    }

    /// Deletes a Discord channel or category (`DELETE /channels/{id}`).
    pub(super) fn delete_channel(&self, channel_id: &str) -> Result<(), Refusal> {
        let Some(guild) = self.guild_of_channel_or_category(channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        self.require_channel_perm(&guild, channel_id, MANAGE_CHANNELS, "delete this channel")?;
        self.call("DELETE", &format!("/channels/{channel_id}"), None).map_err(refusal)?;
        let gone = Channel {
            id: channel_id.to_owned(),
            guild_id: Some(guild),
            ..Channel::default()
        };
        self.channel_deleted(gone).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_channel_body_maps_kinds_parent_and_private_overwrite() {
        let text = create_channel_body("4000", "1000", " announcements ", ConversationKind::Announcement, false, Some("4050")).unwrap();
        assert_eq!(text["name"], "announcements");
        assert_eq!(text["type"], model::NEWS);
        assert_eq!(text["parent_id"], "4050");
        assert!(text.get("permission_overwrites").is_none());

        let voice = create_channel_body("4000", "1000", "Lounge", ConversationKind::Voice, true, None).unwrap();
        assert_eq!(voice["type"], model::VOICE);
        assert!(voice.get("parent_id").is_none());
        let overwrites = voice["permission_overwrites"].as_array().unwrap();
        assert_eq!(overwrites.len(), 2);
        assert_eq!(overwrites[0]["id"], "4000");
        assert_eq!(overwrites[0]["type"], 0);
        assert_eq!(overwrites[0]["deny"], VIEW_CHANNEL.to_string());
        assert_eq!(overwrites[1]["id"], "1000");
        assert_eq!(overwrites[1]["type"], 1);
        assert_eq!(overwrites[1]["allow"], VIEW_CHANNEL.to_string());

        let forum = create_channel_body("4000", "1000", "help", ConversationKind::Forum, false, None).unwrap();
        assert_eq!(forum["type"], model::FORUM);

        assert!(create_channel_body("4000", "1000", "   ", ConversationKind::Channel, false, None).is_err());
        assert!(create_channel_body("4000", "1000", "dm", ConversationKind::Direct, false, None).is_err());
    }

    #[test]
    fn create_category_body_sets_type_4_and_validates_name() {
        let cat = create_category_body(" Projects ").unwrap();
        assert_eq!(cat, json!({"name": "Projects", "type": 4}));
        assert!(create_category_body("   ").is_err());
    }

    #[test]
    fn edit_channel_body_sends_only_changed_fields_and_clears_parent_with_null() {
        let body = edit_channel_body(
            Some("new-name"),
            Some("New topic"),
            Some(30),
            Some(true),
            Some(10),
            Some(None),
        )
        .unwrap();
        assert_eq!(body["name"], "new-name");
        assert_eq!(body["topic"], "New topic");
        assert_eq!(body["rate_limit_per_user"], 30);
        assert_eq!(body["nsfw"], true);
        assert_eq!(body["user_limit"], 10);
        assert_eq!(body["parent_id"], Value::Null);

        let cleared_topic = edit_channel_body(None, Some("   "), None, None, None, Some(Some("4050"))).unwrap();
        assert_eq!(cleared_topic, json!({"topic": null, "parent_id": "4050"}));

        assert!(edit_channel_body(None, None, None, None, None, None).is_err());
        assert!(edit_channel_body(None, None, Some(25_000), None, None, None).is_err());
        assert!(edit_channel_body(None, None, None, None, Some(100), None).is_err());
    }

    #[test]
    fn move_channels_body_builds_single_bulk_array() {
        let body = move_channels_body(&[
            ChannelPosition {
                id: "4101".into(),
                position: 0,
                parent_id: Some(Some("4050".into())),
                lock_permissions: Some(true),
            },
            ChannelPosition {
                id: "4102".into(),
                position: 1,
                parent_id: Some(None),
                lock_permissions: None,
            },
        ])
        .unwrap();
        assert_eq!(
            body,
            json!([
                {"id": "4101", "position": 0, "parent_id": "4050", "lock_permissions": true},
                {"id": "4102", "position": 1, "parent_id": null}
            ])
        );
    }
}
