//! Discord server & channel management: creating, renaming, editing, reordering, and deleting
//! channels and categories (#410).

use super::super::Refusal;
use super::model::{self, number, Channel, Kept, ADMINISTRATOR, MANAGE_CHANNELS, MANAGE_ROLES, VIEW_CHANNEL};
use super::{ready, refusal, Discord};
use crate::hub::now;
use protocol::{
    would_lock_self_out, ChannelOverwrite, ChannelPermissionsInfo, ChannelPosition, ConversationKind, Group,
    OverwriteTargetKind, SpaceRoleSummary,
};
use serde_json::{json, Map, Value};

/// Validates and maps a channel name (trimmed, 1..=100 chars).
/// A store or session failure while applying a change we just made, as the request's refusal.
fn failed(err: super::super::Failure) -> Refusal {
    match err {
        super::super::Failure::SignedOut(why) | super::super::Failure::Dropped(why) => Refusal::Refused(why),
    }
}

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
        self.channel_changed(channel).map_err(failed)?;
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
        self.channel_changed(channel).map_err(failed)?;
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
        self.channel_deleted(gone).map_err(failed)?;
        Ok(())
    }

    /// Reads a channel's or category's permission overwrites and role list (`#397`).
    pub(super) fn channel_permissions(&self, channel_id: &str) -> Result<ChannelPermissionsInfo, Refusal> {
        let Some(guild) = self.guild_of_channel_or_category(channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        self.require_channel_perm(&guild, channel_id, MANAGE_ROLES, "manage permissions for this channel")?;
        let kept_list: Vec<Kept> = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let channel = match kept_list.iter().find(|k| k.id == channel_id) {
            Some(found) => Channel {
                id: found.id.clone(),
                kind: found.kind,
                name: found.name.clone(),
                position: found.position,
                user_limit: found.limit,
                bitrate: found.bitrate,
                parent_id: found.parent.clone(),
                topic: found.topic.clone(),
                permission_overwrites: found
                    .overwrites
                    .iter()
                    .map(|(id, member, allow, deny)| model::Overwrite {
                        id: id.clone(),
                        member: *member,
                        allow: *allow,
                        deny: *deny,
                    })
                    .collect(),
                ..Channel::default()
            },
            None => {
                let raw = self.call("GET", &format!("/channels/{channel_id}"), None).map_err(refusal)?;
                serde_json::from_str(&raw)
                    .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?
            }
        };
        let parent_id = channel.parent_id.clone().filter(|p| !p.is_empty());
        let parent_kept = parent_id.as_deref().and_then(|pid| kept_list.iter().find(|k| k.id == pid));
        let current_tuples: Vec<(String, bool, u64, u64)> = channel
            .permission_overwrites
            .iter()
            .map(|o| (o.id.clone(), o.member, o.allow, o.deny))
            .collect();
        let synced_with_category = parent_kept.map(|parent| overwrites_equal(&current_tuples, &parent.overwrites));

        let (name, is_category, private, my_permissions, is_owner, guild_permissions, my_roles, me_id, parent_name, roles, member_names) = {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(&guild) else {
                return Err(Refusal::Refused("That server is no longer available.".to_owned()));
            };
            let me = state.me.clone();
            let name = channel.name.clone().unwrap_or_else(|| channel_id.to_owned());
            let is_category = channel.kind == model::CATEGORY;
            let private = info.private(&guild, &channel);
            let my_permissions = info.permissions(&guild, &me, &channel);
            let guild_permissions = info.guild_permissions(&guild);
            let my_roles: Vec<String> = info.my_roles.iter().map(u64::to_string).collect();
            let parent_name = parent_id
                .as_deref()
                .and_then(|pid| info.categories.get(pid).map(|(_, n)| n.clone()));
            let mut roles: Vec<SpaceRoleSummary> = info
                .roles
                .iter()
                .map(|(rid, r)| SpaceRoleSummary {
                    id: rid.to_string(),
                    name: r.name.clone(),
                    permissions: r.permissions,
                })
                .collect();
            roles.sort_by(|a, b| {
                let a_ev = a.id == guild;
                let b_ev = b.id == guild;
                b_ev.cmp(&a_ev).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            let mut member_names = std::collections::HashMap::new();
            for ov in &channel.permission_overwrites {
                if ov.member {
                    if let Some(n) = state.users.get(&ov.id) {
                        member_names.insert(ov.id.clone(), n.clone());
                    }
                }
            }
            (
                name,
                is_category,
                private,
                my_permissions,
                info.owner,
                guild_permissions,
                my_roles,
                me,
                parent_name,
                roles,
                member_names,
            )
        };

        let mut overwrites: Vec<ChannelOverwrite> = channel
            .permission_overwrites
            .iter()
            .map(|ov| {
                let kind = if ov.member { OverwriteTargetKind::Member } else { OverwriteTargetKind::Role };
                let label = if ov.member {
                    member_names
                        .get(&ov.id)
                        .cloned()
                        .or_else(|| self.ctx.hub.store().person(&self.ctx.key(&ov.id)).ok().flatten().map(|p| p.name))
                        .unwrap_or_else(|| format!("Member {}", ov.id))
                } else if ov.id == guild {
                    "@everyone".to_owned()
                } else {
                    roles
                        .iter()
                        .find(|r| r.id == ov.id)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| format!("Role {}", ov.id))
                };
                ChannelOverwrite {
                    id: ov.id.clone(),
                    kind,
                    name: label,
                    allow: ov.allow,
                    deny: ov.deny,
                }
            })
            .collect();
        overwrites.sort_by(|a, b| {
            let a_ev = a.id == guild && a.kind == OverwriteTargetKind::Role;
            let b_ev = b.id == guild && b.kind == OverwriteTargetKind::Role;
            b_ev.cmp(&a_ev)
                .then_with(|| (a.kind != OverwriteTargetKind::Role).cmp(&(b.kind != OverwriteTargetKind::Role)))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(ChannelPermissionsInfo {
            conversation: self.ctx.key(channel_id),
            name,
            is_category,
            private,
            synced_with_category,
            parent_id,
            parent_name,
            my_permissions,
            is_owner,
            guild_permissions,
            my_roles,
            me_id,
            everyone_role_id: guild,
            overwrites,
            roles,
        })
    }

    /// Creates or updates one permission overwrite (`PUT /channels/{id}/permissions/{target}`).
    pub(super) fn set_overwrite(
        &self,
        channel_id: &str,
        target_id: &str,
        target_kind: OverwriteTargetKind,
        allow: u64,
        deny: u64,
    ) -> Result<(), Refusal> {
        let target_id = target_id.trim();
        if target_id.is_empty() {
            return Err(Refusal::Refused("Choose a role or member for this overwrite.".to_owned()));
        }
        let Some(guild) = self.guild_of_channel_or_category(channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        self.require_channel_perm(&guild, channel_id, MANAGE_ROLES, "edit permissions for this channel")?;
        // Server-side self-lockout check so direct requests cannot strip our own access.
        let current = self.channel_permissions(channel_id)?;
        if would_lock_self_out(
            current.is_owner,
            current.guild_permissions,
            &current.me_id,
            &current.everyone_role_id,
            &current.my_roles,
            &current.overwrites,
            target_id,
            target_kind,
            Some((allow, deny)),
        ) {
            return Err(Refusal::Refused(
                "That change would lock you out of viewing or managing permissions on this channel.".to_owned(),
            ));
        }
        let body = set_overwrite_body(target_kind, allow, deny);
        self.call("PUT", &format!("/channels/{channel_id}/permissions/{target_id}"), Some(body))
            .map_err(refusal)?;
        let is_member = target_kind == OverwriteTargetKind::Member;
        self.keep(&guild, |kept| {
            if let Some(ch) = kept.iter_mut().find(|k| k.id == channel_id) {
                match ch.overwrites.iter_mut().find(|(id, m, _, _)| id == target_id && *m == is_member) {
                    Some(existing) => {
                        existing.2 = allow;
                        existing.3 = deny;
                    }
                    None => ch.overwrites.push((target_id.to_owned(), is_member, allow, deny)),
                }
            }
        });
        self.replant(&guild).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(())
    }

    /// Removes one permission overwrite (`DELETE /channels/{id}/permissions/{target}`).
    pub(super) fn delete_overwrite(&self, channel_id: &str, target_id: &str) -> Result<(), Refusal> {
        let target_id = target_id.trim();
        if target_id.is_empty() {
            return Err(Refusal::Refused("Overwrite target id cannot be empty.".to_owned()));
        }
        let Some(guild) = self.guild_of_channel_or_category(channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        self.require_channel_perm(&guild, channel_id, MANAGE_ROLES, "edit permissions for this channel")?;
        let current = self.channel_permissions(channel_id)?;
        if let Some(existing) = current.overwrites.iter().find(|o| o.id == target_id) {
            if would_lock_self_out(
                current.is_owner,
                current.guild_permissions,
                &current.me_id,
                &current.everyone_role_id,
                &current.my_roles,
                &current.overwrites,
                target_id,
                existing.kind,
                None,
            ) {
                return Err(Refusal::Refused(
                    "Removing that overwrite would lock you out of viewing or managing permissions on this channel.".to_owned(),
                ));
            }
        }
        self.call("DELETE", &format!("/channels/{channel_id}/permissions/{target_id}"), None)
            .map_err(refusal)?;
        self.keep(&guild, |kept| {
            if let Some(ch) = kept.iter_mut().find(|k| k.id == channel_id) {
                ch.overwrites.retain(|(id, _, _, _)| id != target_id);
            }
        });
        self.replant(&guild).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(())
    }

    /// Copies the parent category's permission overwrites onto a channel (`PATCH /channels/{id}`).
    pub(super) fn sync_channel_permissions(&self, channel_id: &str) -> Result<(), Refusal> {
        let Some(guild) = self.guild_of_channel_or_category(channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        self.require_channel_perm(&guild, channel_id, MANAGE_ROLES, "sync permissions for this channel")?;
        let kept_list: Vec<Kept> = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let Some(channel) = kept_list.iter().find(|k| k.id == channel_id) else {
            return Err(Refusal::Refused("That channel is no longer available.".to_owned()));
        };
        let Some(parent_id) = channel.parent.as_deref().filter(|p| !p.is_empty()) else {
            return Err(Refusal::Refused("This channel is not in a category.".to_owned()));
        };
        let Some(parent) = kept_list.iter().find(|k| k.id == parent_id) else {
            return Err(Refusal::Refused("That channel's parent category is not available.".to_owned()));
        };
        let synced = parent.overwrites.clone();
        let body = sync_overwrites_body(&synced);
        self.call("PATCH", &format!("/channels/{channel_id}"), Some(body)).map_err(refusal)?;
        self.keep(&guild, |kept| {
            if let Some(ch) = kept.iter_mut().find(|k| k.id == channel_id) {
                ch.overwrites = synced;
            }
        });
        self.replant(&guild).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(())
    }
}

/// Builds the JSON body for `PUT /channels/{id}/permissions/{target}` with 64-bit decimal strings.
pub(super) fn set_overwrite_body(target_kind: OverwriteTargetKind, allow: u64, deny: u64) -> Value {
    let kind_num = match target_kind {
        OverwriteTargetKind::Role => 0u8,
        OverwriteTargetKind::Member => 1u8,
    };
    json!({
        "type": kind_num,
        "allow": allow.to_string(),
        "deny": deny.to_string(),
    })
}

/// Builds the JSON body for `PATCH /channels/{id}` when syncing overwrites with a parent category.
pub(super) fn sync_overwrites_body(overwrites: &[(String, bool, u64, u64)]) -> Value {
    let list: Vec<Value> = overwrites
        .iter()
        .map(|(id, member, allow, deny)| {
            json!({
                "id": id,
                "type": if *member { 1u8 } else { 0u8 },
                "allow": allow.to_string(),
                "deny": deny.to_string(),
            })
        })
        .collect();
    json!({ "permission_overwrites": list })
}

/// Compares two channel overwrite lists ignoring ordering.
pub(super) fn overwrites_equal(a: &[(String, bool, u64, u64)], b: &[(String, bool, u64, u64)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut left = a.to_vec();
    let mut right = b.to_vec();
    left.sort();
    right.sort();
    left == right
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

    #[test]
    fn set_overwrite_and_sync_bodies_encode_64bit_decimal_strings() {
        let high_bit = (1u64 << 51) | (1u64 << 60);
        let body = set_overwrite_body(OverwriteTargetKind::Member, high_bit, 1u64 << 10);
        assert_eq!(body["type"], 1);
        assert_eq!(body["allow"], high_bit.to_string());
        assert_eq!(body["deny"], "1024");

        let sync = sync_overwrites_body(&[
            ("4000".into(), false, 0, 1024),
            ("2001".into(), true, 1024, 0),
        ]);
        assert_eq!(
            sync,
            json!({
                "permission_overwrites": [
                    {"id": "4000", "type": 0, "allow": "0", "deny": "1024"},
                    {"id": "2001", "type": 1, "allow": "1024", "deny": "0"}
                ]
            })
        );
        assert!(overwrites_equal(
            &[("4000".into(), false, 0, 1024), ("2001".into(), true, 1024, 0)],
            &[("2001".into(), true, 1024, 0), ("4000".into(), false, 0, 1024)],
        ));
        assert!(!overwrites_equal(&[("4000".into(), false, 0, 1024)], &[]));
    }
}
