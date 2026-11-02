//! The gateway's events after READY: messages, reactions, read state, people, typing.

use super::super::Failure;
use super::content::{self, Place};
use super::model::{self, number, seconds, Channel, Emoji, Member, Posted, Relationship, Role, User};
use super::{proto, Discord, Lookup};
use protocol::{ConversationKind, Reaction, Relation};
use serde::Deserialize;
use serde_json::from_str;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

/// Seconds a typing line lasts with nothing more heard, as in Discord's own client.
const TYPING_SECONDS: i64 = 10;

#[derive(Deserialize)]
struct Removed {
    #[serde(default)]
    id: String,
    #[serde(default)]
    ids: Vec<String>,
    channel_id: String,
}

#[derive(Deserialize)]
struct Reacting {
    #[serde(default)]
    user_id: String,
    channel_id: String,
    message_id: String,
    #[serde(default)]
    emoji: Option<Emoji>,
    /// `MESSAGE_REACTION_ADD_MANY`: several people at once. Its shape is from the community
    /// documentation and unconfirmed against a real account.
    #[serde(default)]
    reactions: Vec<ReactingMany>,
}

#[derive(Deserialize)]
struct ReactingMany {
    #[serde(default)]
    users: Vec<String>,
    emoji: Emoji,
}

#[derive(Deserialize)]
struct Acked {
    channel_id: String,
    message_id: String,
    #[serde(default)]
    mention_count: Option<u32>,
}

#[derive(Deserialize)]
struct Typing {
    channel_id: String,
    user_id: String,
    #[serde(default)]
    member: Option<Member>,
}

#[derive(Deserialize)]
struct Newest {
    id: String,
    #[serde(default)]
    last_message_id: Option<String>,
}

/// `CHANNEL_UNREAD_UPDATE` and `PASSIVE_UPDATE_V2`: what moved in a server we are not subscribed
/// to, without the messages themselves.
#[derive(Deserialize)]
struct Passive {
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    channel_unread_updates: Vec<Newest>,
    #[serde(default)]
    updated_channels: Vec<Newest>,
    #[serde(default)]
    updated_voice_states: Vec<model::VoiceState>,
    #[serde(default)]
    removed_voice_states: Vec<String>,
}

#[derive(Deserialize)]
struct ThreadList {
    #[serde(default)]
    threads: Vec<Channel>,
}

#[derive(Deserialize)]
struct SettingsProto {
    #[serde(default)]
    proto: String,
    #[serde(rename = "type", default)]
    kind: u8,
}

#[derive(Deserialize)]
struct SettingsUpdate {
    settings: SettingsProto,
    #[serde(default)]
    partial: bool,
}

#[derive(Deserialize)]
struct RoleChange {
    guild_id: String,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    role_id: Option<String>,
}

#[derive(Deserialize)]
struct MemberChange {
    guild_id: String,
    #[serde(flatten)]
    member: Member,
}

#[derive(Deserialize)]
struct RequiredAction {
    #[serde(default)]
    required_action: Option<String>,
}

impl Discord {
    pub(super) fn dispatch(&self, event: &str, data: &str) -> Result<(), Failure> {
        match event {
            "READY_SUPPLEMENTAL" => self.supplemental(from_str(data)?),
            "MESSAGE_CREATE" => self.created(from_str(data)?)?,
            "MESSAGE_UPDATE" => self.updated(from_str(data)?)?,
            "MESSAGE_DELETE" | "MESSAGE_DELETE_BULK" => {
                let removed: Removed = from_str(data)?;
                for id in removed.ids.iter().chain(Some(&removed.id)).filter(|id| !id.is_empty()) {
                    self.ctx.deleted(&removed.channel_id, id)?;
                }
            }
            "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_ADD_MANY" => self.reacted(from_str(data)?, true)?,
            "MESSAGE_REACTION_REMOVE" => self.reacted(from_str(data)?, false)?,
            "MESSAGE_REACTION_REMOVE_ALL" | "MESSAGE_REACTION_REMOVE_EMOJI" => self.unreacted(from_str(data)?)?,
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" => self.channel_changed(from_str(data)?)?,
            "CHANNEL_DELETE" | "THREAD_DELETE" => self.channel_deleted(from_str(data)?)?,
            "THREAD_CREATE" | "THREAD_UPDATE" => self.thread_changed(from_str(data)?, false)?,
            "THREAD_LIST_SYNC" => {
                let list: ThreadList = from_str(data)?;
                for thread in list.threads {
                    self.thread_changed(thread, true)?;
                }
            }
            "GUILD_CREATE" => self.guild_created(from_str(data)?)?,
            "GUILD_UPDATE" => self.guild_updated(from_str(data)?)?,
            "GUILD_DELETE" => self.guild_deleted(from_str(data)?)?,
            "MESSAGE_ACK" => self.acked(from_str(data)?)?,
            "USER_GUILD_SETTINGS_UPDATE" => self.settings_changed(from_str(data)?)?,
            "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" => self.related(from_str(data)?)?,
            "RELATIONSHIP_REMOVE" => {
                let gone: Relationship = from_str(data)?;
                let mut state = self.state.lock().unwrap();
                state.relations.remove(&gone.id);
                state.rows.remove(&gone.id);
                state.presence.remove(&gone.id);
                drop(state);
                self.ctx.relation(&gone.id, Relation::None)?;
            }
            "PRESENCE_UPDATE" => self.presence(&from_str(data)?),
            "TYPING_START" => self.typing_started(from_str(data)?),
            "CHANNEL_UNREAD_UPDATE" | "PASSIVE_UPDATE_V2" => self.passive(from_str(data)?)?,
            "USER_UPDATE" => {
                let user: User = from_str(data)?;
                // Ours: a new name or picture.
                if user.id == self.me() {
                    self.myself(&user)?;
                }
            }
            "USER_SETTINGS_PROTO_UPDATE" => self.settings_proto(from_str(data)?)?,
            // Discord wants a phone number, an email check or a captcha: nothing more is asked of
            // it until the user has dealt with that in Discord's own app.
            "USER_REQUIRED_ACTION_UPDATE" => {
                let action: RequiredAction = from_str(data)?;
                match action.required_action.as_deref() {
                    Some(action) => {
                        let what = match action {
                            action if action.contains("PHONE") => "add or verify a phone number",
                            action if action.contains("EMAIL") => "verify your e-mail address",
                            action if action.contains("CAPTCHA") => "pass a captcha",
                            _ => "verify your account",
                        };
                        self.rest.challenge(&format!("Discord wants you to {what}. Do that in Discord; tinyclient asks nothing more until then."));
                    }
                    None => {
                        self.rest.challenged.store(false, Ordering::Relaxed);
                        self.ctx.attention("");
                    }
                }
            }
            // Who may see which channel follows the roles and our own membership.
            "GUILD_ROLE_CREATE" | "GUILD_ROLE_UPDATE" | "GUILD_ROLE_DELETE" => {
                let change: RoleChange = from_str(data)?;
                self.role_changed(&change.guild_id, change.role, change.role_id.as_deref())?;
            }
            "GUILD_MEMBER_UPDATE" => {
                let change: MemberChange = from_str(data)?;
                self.member_changed(&change.guild_id, &change.member)?;
            }
            "GUILD_MEMBER_LIST_UPDATE" => self.member_list(from_str(data)?),
            "GUILD_MEMBERS_CHUNK" => self.members_chunk(from_str(data)?)?,
            "VOICE_STATE_UPDATE" => self.voice_state(from_str(data)?),
            "GUILD_EMOJIS_UPDATE" | "GUILD_STICKERS_UPDATE" => self.pickables_changed(from_str(data)?),
            "VOICE_SERVER_UPDATE" => self.voice_server(from_str(data)?),
            "CALL_CREATE" | "CALL_UPDATE" => self.call_changed(from_str(data)?),
            "CALL_DELETE" => self.call_deleted(from_str(data)?),
            _ => {}
        }
        Ok(())
    }

    /// A new message. Direct conversations, threads and anything that names us are always kept.
    /// A server channel's messages are kept once the channel has been opened; until then only
    /// its unread state moves, so busy servers do not fill the store.
    fn created(&self, posted: Posted) -> Result<(), Failure> {
        let channel = posted.channel_id.clone();
        let conversation = match self.ctx.known(&channel)? {
            Some(conversation) => conversation,
            // A hidden channel, or one of a server whose tree we do not have.
            None if posted.guild_id.is_some() => return Ok(()),
            // Someone we have no conversation with yet wrote first.
            None => {
                let me = self.me();
                let Some(author) = posted.author.clone().filter(|author| author.id != me) else {
                    return Ok(());
                };
                let started = Channel {
                    id: channel.clone(),
                    kind: model::DM,
                    recipients: vec![author],
                    ..Channel::default()
                };
                self.direct(&started, &HashMap::new())?;
                match self.ctx.known(&channel)? {
                    Some(conversation) => conversation,
                    None => return Ok(()),
                }
            }
        };
        let guild = posted.guild_id.clone().or_else(|| self.guild_of(&conversation));
        let (message, files) = self.convert(&posted, guild.as_deref(), conversation.kind);
        let always = conversation.kind.is_direct() || conversation.kind == ConversationKind::Thread;
        if always || message.mentions_me || message.outgoing || self.ctx.newest(&channel)?.is_some() {
            files.register(&self.ctx)?;
            self.thread_of(&posted, &conversation)?;
            self.ctx.message(&channel, &message)?;
        } else {
            self.ctx.counts(&channel, conversation.unread.max(1), conversation.mentions, None, message.sent)?;
            self.ctx.typing_stopped(&channel, &message.sender.id);
        }
        Ok(())
    }

    /// An edit, a link preview arriving, a pin. Only a message that is stored is touched: an
    /// update must never appear as a new unread message.
    pub(super) fn updated(&self, posted: Posted) -> Result<(), Failure> {
        let channel = &posted.channel_id;
        let Some(mut stored) = self.ctx.stored_message(channel, &posted.id)? else {
            return Ok(());
        };
        let Some(conversation) = self.ctx.known(channel)? else {
            return Ok(());
        };
        let guild = posted.guild_id.clone().or_else(|| self.guild_of(&conversation));
        let files = if posted.author.is_some() {
            // Whole: translate it again, keeping what only the first arrival knew.
            let (mut message, files) = self.convert(&posted, guild.as_deref(), conversation.kind);
            message.mentions_me = stored.mentions_me;
            message.client_id = stored.client_id.take();
            if posted.reactions.is_none() {
                message.reactions = std::mem::take(&mut stored.reactions);
            }
            if message.thread.is_none() {
                message.thread = stored.thread.take();
            }
            stored = message;
            files
        } else {
            let me = self.me();
            let lookup = Lookup {
                discord: self,
                guild: guild.as_deref(),
                mentions: &posted.mentions,
            };
            let place = Place {
                account: &self.ctx.account,
                me: &me,
                guild: guild.as_deref(),
                names: &lookup,
                author: None,
            };
            content::patch(&mut stored, &posted, &place)
        };
        files.register(&self.ctx)?;
        self.ctx.message(channel, &stored)?;
        Ok(())
    }

    fn reacted(&self, reacting: Reacting, added: bool) -> Result<(), Failure> {
        let me = self.me();
        let single = reacting.emoji.iter().map(|emoji| (emoji, std::slice::from_ref(&reacting.user_id)));
        let many = reacting.reactions.iter().map(|many| (&many.emoji, many.users.as_slice()));
        for (emoji, users) in single.chain(many) {
            let (spelled, file) = emoji.spelled();
            if let Some(file) = &file {
                self.ctx.media(std::slice::from_ref(file))?;
            }
            for user in users {
                let reaction = Reaction {
                    emoji: spelled.clone(),
                    file: file.as_ref().map(|(file, _)| file.clone()),
                    count: 1,
                    mine: *user == me,
                };
                self.ctx.reacted(&reacting.channel_id, &reacting.message_id, &reaction, added)?;
            }
        }
        Ok(())
    }

    /// Every reaction removed, or every one of a single emoji.
    fn unreacted(&self, removed: Reacting) -> Result<(), Failure> {
        let Some(mut stored) = self.ctx.stored_message(&removed.channel_id, &removed.message_id)? else {
            return Ok(());
        };
        match removed.emoji.as_ref().map(|emoji| emoji.spelled().0) {
            Some(emoji) => stored.reactions.retain(|reaction| reaction.emoji != emoji),
            None => stored.reactions.clear(),
        }
        Ok(self.ctx.message(&removed.channel_id, &stored)?)
    }

    /// Read on another device (or acknowledged from here): the count goes, and the marker moves.
    fn acked(&self, acked: Acked) -> Result<(), Failure> {
        let Some(conversation) = self.ctx.known(&acked.channel_id)? else {
            return Ok(());
        };
        // An ack of a marker we have already passed is Discord repeating the old one. Applying
        // it would unread a blocked DM whose ack Discord refused.
        if let Some(last) = conversation.last_read.as_deref().filter(|id| !id.is_empty()) {
            let ack_n = number(&acked.message_id);
            let last_n = number(last);
            let already = conversation.unread == 0 && conversation.mentions == 0;
            if last_n > ack_n || (last_n >= ack_n && already) {
                return Ok(());
            }
        }
        let caught_up = seconds(&acked.message_id) >= conversation.updated;
        let mentions = acked.mention_count.unwrap_or(0);
        let (unread, mentions) = match (caught_up, conversation.kind.is_direct()) {
            (true, _) => (0, 0),
            (false, true) => (mentions.max(1), 0),
            (false, false) => (1, mentions),
        };
        self.ctx.counts(&acked.channel_id, unread, mentions, Some(&acked.message_id), conversation.updated)?;
        Ok(())
    }

    fn related(&self, relationship: Relationship) -> Result<(), Failure> {
        let Some(user) = &relationship.user else {
            return Ok(());
        };
        let row = user.person(relationship.nickname.as_deref());
        {
            let mut state = self.state.lock().unwrap();
            state.users.insert(user.id.clone(), row.name.clone());
            state.rows.insert(user.id.clone(), row.clone());
            state.relations.insert(user.id.clone(), relationship.relation());
        }
        self.ctx.media(&[user.avatar()])?;
        self.ctx.people(&[row])?;
        self.ctx.relation(&user.id, relationship.relation())?;
        Ok(())
    }

    fn typing_started(&self, typing: Typing) {
        if typing.user_id == self.me() {
            return;
        }
        let named = typing.member.as_ref().map(|member| member.name().to_owned()).filter(|name| !name.is_empty());
        let named = named.or_else(|| self.state.lock().unwrap().users.get(&typing.user_id).cloned());
        let name = named.unwrap_or_else(|| "Someone".to_owned());
        self.ctx.typing(&typing.channel_id, &typing.user_id, &name, TYPING_SECONDS);
    }

    /// Channels of a server we hold no subscription for got newer messages: their rows turn
    /// unread without the messages being fetched.
    fn passive(&self, passive: Passive) -> Result<(), Failure> {
        for newest in passive.channel_unread_updates.iter().chain(&passive.updated_channels) {
            let (Some(last), Some(conversation)) = (&newest.last_message_id, self.ctx.known(&newest.id)?) else {
                continue;
            };
            let listed = matches!(
                conversation.kind,
                ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Thread
            );
            let read = conversation.last_read.as_deref().is_some_and(|read| number(read) >= number(last));
            if listed && !read && seconds(last) > conversation.updated {
                self.ctx.counts(&newest.id, conversation.unread.max(1), conversation.mentions, None, seconds(last))?;
            }
        }
        if !passive.updated_voice_states.is_empty() || !passive.removed_voice_states.is_empty() {
            self.voice_states(passive.guild_id.as_deref(), &passive.updated_voice_states);
            self.voice_left(&passive.removed_voice_states);
        }
        Ok(())
    }

    /// The user changed their status or reordered their servers, on any device. A partial
    /// update carries only what changed, so the status is read from either kind and the order
    /// only from a whole one.
    fn settings_proto(&self, update: SettingsUpdate) -> Result<(), Failure> {
        if update.settings.kind != 1 {
            return Ok(());
        }
        if let Some(status) = proto::status_settings(&update.settings.proto) {
            self.own_status(status);
        }
        if update.partial {
            return Ok(());
        }
        if let Some(folders) = proto::guild_folders(&update.settings.proto) {
            self.record_folders(&folders)?;
        }
        let Some(order) = proto::guild_order(&update.settings.proto) else {
            return Ok(());
        };
        {
            let mut state = self.state.lock().unwrap();
            for (id, info) in &mut state.guilds {
                if let Some(position) = order.iter().position(|placed| placed == id) {
                    info.position = position as i32;
                }
            }
        }
        Ok(self.spaces()?)
    }
}
