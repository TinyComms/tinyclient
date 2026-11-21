//! A chat's or channel's info pane, its mute, and whether the other side has seen my last
//! message. Each is one request to the chat service, made because the user opened something.
//!
//! Unverified on a real tenant, most likely wrong first:
//! - Mute is the chat service's `alerts` property, as Skype's web client writes it; the Teams
//!   client may keep its own notification setting elsewhere (no client read sets it).
//! - "Seen" is read from `GET /v1/threads/{id}/consumptionhorizons`, the members' read markers.
//! - A thread's `createdat` and `creator` properties, read off the thread description.

use super::links;
use super::{escaped, id_of, millis, now_ms, seconds, thread_of, Body, Fault, Teams};
use crate::adapters::{ConversationAbout, SpaceAbout};
use protocol::{ConversationKind, Message, Notify};
use serde_json::{json, Value};

/// The user's personal Saved messages stream on the Teams chat service.
pub(super) const SAVED_STREAM: &str = "48:saved";

/// One entry in a thread's `properties.pinnedMessages` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinnedEntry {
    pub message_id: String,
    pub pinned_by: Option<String>,
    pub pinned_at: Option<i64>,
}

/// Parses Teams thread `properties.pinnedMessages` or `properties.pinnedMessageIds`
/// (array or JSON-encoded string of objects `{messageId, pinnedBy, pinnedTime}` or ids).
pub(super) fn parse_pinned_messages(props: &Value) -> Vec<PinnedEntry> {
    let raw_list = |val: &Value| -> Vec<Value> {
        match val {
            Value::Array(items) => items.clone(),
            Value::String(s) => serde_json::from_str::<Vec<Value>>(s).unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    let mut entries = raw_list(&props["pinnedMessages"]);
    if entries.is_empty() {
        entries = raw_list(&props["pinnedMessageIds"]);
    }
    let mut out = Vec::new();
    for item in entries {
        let entry = match &item {
            Value::String(s) if !s.trim().is_empty() => PinnedEntry {
                message_id: s.trim().to_owned(),
                pinned_by: None,
                pinned_at: None,
            },
            Value::Number(n) => PinnedEntry {
                message_id: n.to_string(),
                pinned_by: None,
                pinned_at: None,
            },
            Value::Object(_) => {
                let message_id = [&item["messageId"], &item["id"], &item["message_id"]]
                    .into_iter()
                    .map(id_of)
                    .find(|id| !id.is_empty())
                    .unwrap_or_default();
                if message_id.is_empty() {
                    continue;
                }
                let pinned_by = [
                    &item["pinnedByDisplayName"],
                    &item["pinnedByName"],
                    &item["pinnedBy"],
                    &item["pinnedByMri"],
                ]
                .into_iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|s| !s.is_empty())
                .map(str::to_owned);
                let pinned_at = [&item["pinnedTime"], &item["pinnedAt"], &item["timestamp"]]
                    .into_iter()
                    .find_map(|v| match v {
                        Value::Number(n) => n.as_i64().map(|t| if t > 10_000_000_000 { t / 1000 } else { t }),
                        Value::String(s) => {
                            let ms = millis(s);
                            if ms > 10_000_000_000 {
                                Some(ms / 1000)
                            } else if ms > 0 {
                                Some(ms)
                            } else {
                                let sec = seconds(s);
                                (sec > 0).then_some(sec)
                            }
                        }
                        _ => None,
                    })
                    .filter(|&t| t > 0);
                PinnedEntry { message_id, pinned_by, pinned_at }
            }
            _ => continue,
        };
        if !out.iter().any(|existing: &PinnedEntry| existing.message_id == entry.message_id) {
            out.push(entry);
        }
    }
    out
}

/// Serializes pinned entries back into Teams' `properties.pinnedMessages` JSON string format.
fn encode_pinned_messages(entries: &[PinnedEntry]) -> String {
    let arr: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut obj = json!({"messageId": entry.message_id});
            if let Some(ref by) = entry.pinned_by {
                obj["pinnedBy"] = Value::String(by.clone());
            }
            if let Some(at) = entry.pinned_at {
                obj["pinnedTime"] = json!(at.saturating_mul(1000));
            }
            obj
        })
        .collect();
    Value::Array(arr).to_string()
}

/// "Seen" is told in one-to-one and small chats only, never in channels, as Teams does.
const SMALL: usize = 20;

pub(super) fn notify_word(notify: Notify) -> &'static str {
    match notify {
        Notify::All => "all",
        Notify::Mentions => "mentions",
        Notify::Nothing => "nothing",
    }
}

pub(super) fn parse_notify_word(word: &str) -> Option<Notify> {
    match word {
        "all" => Some(Notify::All),
        "mentions" => Some(Notify::Mentions),
        "nothing" => Some(Notify::Nothing),
        _ => None,
    }
}

/// Reads Teams per-conversation notification level from `channelNotificationSettings`,
/// `alerts` + `mentions`, `isMuted`, or `isFollowed`.
pub(super) fn parse_notify(source: &Value, is_channel: bool) -> Option<Notify> {
    let settings = match &source["channelNotificationSettings"] {
        Value::String(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    if settings.is_object() {
        if let Some(level) = settings["level"].as_str().or_else(|| settings["mode"].as_str()) {
            match level.to_ascii_lowercase().as_str() {
                "all" | "allposts" | "bannerandfeed" => return Some(Notify::All),
                "mentions" | "mentionsonly" => return Some(Notify::Mentions),
                "off" | "none" | "nothing" => return Some(Notify::Nothing),
                _ => {}
            }
        }
        let all_posts = &settings["allPosts"];
        let new_posts = if all_posts.is_null() { &settings["newPosts"] } else { all_posts };
        let mentions = &settings["mentions"];
        let is_on = |v: &Value| {
            *v == Value::Bool(true)
                || v.as_str().is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "bannerandfeed" | "feed" | "on" | "true" | "all"))
        };
        let is_off = |v: &Value| {
            *v == Value::Bool(false)
                || v.as_str().is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "off" | "none" | "false"))
        };
        if is_on(new_posts) {
            return Some(Notify::All);
        }
        if is_off(new_posts) {
            return Some(if is_off(mentions) { Notify::Nothing } else { Notify::Mentions });
        }
    }

    let alerts = &source["alerts"];
    let mentions = &source["mentions"];
    let mentions_on = *mentions == Value::Bool(true)
        || mentions.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("on"));
    match alerts {
        Value::Bool(true) => return Some(Notify::All),
        Value::Bool(false) => return Some(if mentions_on { Notify::Mentions } else { Notify::Nothing }),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "true" | "all" => return Some(Notify::All),
            "mentions" | "mentionsonly" => return Some(Notify::Mentions),
            "false" | "off" | "none" => return Some(if mentions_on { Notify::Mentions } else { Notify::Nothing }),
            _ => {}
        },
        _ => {}
    }

    if source["isMuted"] == true {
        return Some(Notify::Nothing);
    }
    if is_channel && source["isFollowed"] == true {
        return Some(Notify::All);
    }
    None
}

impl Teams {
    fn described(&self, thread: &str) -> Result<Value, Fault> {
        Ok(self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true)?.body)
    }

    pub(super) fn conversation_info(&self, conversation: &str) -> Result<ConversationAbout, Fault> {
        let thread = thread_of(conversation);
        let row = self.ctx.known(thread)?;
        let channel = super::tree::is_channel(thread);
        let remembered = self
            .state()
            .notify
            .get(thread)
            .copied()
            .or_else(|| self.ctx.note(&format!("notify:{thread}")).as_deref().and_then(parse_notify_word));
        let default_notify = if row.as_ref().is_some_and(|row| row.muted) {
            Notify::Nothing
        } else if let Some(level) = remembered {
            level
        } else {
            Notify::All
        };
        if thread.starts_with("48:") {
            return Ok(ConversationAbout { notify: default_notify, ..ConversationAbout::default() });
        }
        let described = self.described(thread)?;
        let properties = &described["properties"];
        let notify = parse_notify(properties, channel).unwrap_or(default_notify);
        self.state().notify.insert(thread.to_owned(), notify);
        self.ctx.set_note(&format!("notify:{thread}"), notify_word(notify));
        let members = described["members"].as_array().map(|members| members.len() as u32);
        let text = |name: &str| properties[name].as_str().unwrap_or("").trim().to_owned();
        let creator = text("creator");
        let creator = match creator.is_empty() {
            true => String::new(),
            false => {
                self.lookup(std::slice::from_ref(&creator));
                self.name_of(&creator).unwrap_or_default()
            }
        };
        let created = millis(&id_of(&properties["createdat"])) / 1000;
        let shared = properties["sharedChannel"] == true
            || properties["threadType"] == "sharedChannel"
            || row.as_ref().is_some_and(|r| r.shared);
        let external = if shared {
            if row.as_ref().is_some_and(|r| r.mark == Some(protocol::Mark::External)) {
                "External shared channel".to_owned()
            } else {
                "Shared channel".to_owned()
            }
        } else if let Some(peer) = row.as_ref().and_then(|r| r.peer.as_deref()) {
            match self.cached(peer) {
                Some(k) if k.external => {
                    if !k.tenant_name.is_empty() && !k.tenant_name.eq_ignore_ascii_case("Shared channel") {
                        k.tenant_name
                    } else {
                        "External".to_owned()
                    }
                }
                // A non-external guest keeps an empty org string. The row's Guest mark
                // is the label; storing "Guest" here renders as External · Guest.
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let pinned_entries = parse_pinned_messages(properties);
        let pinned_count = Some(pinned_entries.len() as u32);
        self.state().pinned_messages.insert(thread.to_owned(), pinned_entries);
        Ok(ConversationAbout {
            // A chat's topic is its title, already on the row: only a description is more.
            description: text("description"),
            members,
            created: (created > 0).then_some(created),
            creator,
            notify,
            pinned: pinned_count,
            external,
            link: self.share_link(thread, row.as_ref(), shared),
            ..ConversationAbout::default()
        })
    }

    /// The https link the info pane copies. A channel and a chat use Microsoft's documented
    /// forms. A meeting chat uses the join link. A reply chain links its post.
    fn share_link(&self, thread: &str, row: Option<&protocol::Conversation>, shared: bool) -> String {
        if thread.starts_with("19:meeting_") {
            return links::meeting_share(thread);
        }
        match row {
            Some(row) if row.kind == ConversationKind::Channel => {
                let group = row.space.as_ref().and_then(|team| self.ctx.note(&format!("group:{team}"))).unwrap_or_default();
                let mut link = links::channel_share(thread, &row.title, &group, &self.session.tenant);
                if shared {
                    link.push_str("&ngc=true&allowXTenantAccess=true");
                } else if row.private {
                    link.push_str("&ngc=true");
                }
                link
            }
            Some(row) if row.kind == ConversationKind::Thread => match (row.parent.as_deref(), row.root.as_deref()) {
                (Some(parent), Some(root)) => links::message_share(parent, root, &self.session.tenant, None),
                _ => String::new(),
            },
            _ => links::chat_share(thread),
        }
    }

    /// A team's info, from its own thread. The team's id is that thread (General).
    pub(super) fn space_info(&self, space: &str) -> Result<SpaceAbout, Fault> {
        let described = self.described(space)?;
        let properties = &described["properties"];
        let text = |name: &str| properties[name].as_str().unwrap_or("").trim().to_owned();
        let me = self.me();
        let role = described["members"].as_array().into_iter().flatten().find(|member| member["id"].as_str() == Some(me.as_str()));
        let role = role.and_then(|member| member["role"].as_str()).and_then(super::people::role_label);
        let members = described["members"].as_array().map(|members| members.len() as u32);
        let group = self
            .ctx
            .note(&format!("group:{space}"))
            .or_else(|| properties["groupId"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let link = links::team_share(space, &group, &self.session.tenant);
        let mut description = text("description");
        let archived = properties["isArchived"] == true
            || properties["isArchived"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"));
        if archived {
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str("This team is archived (read-only). Manage archive, templates, and moderation settings in Teams.");
        }
        Ok(SpaceAbout {
            description,
            members,
            roles: role.map(str::to_owned).into_iter().collect(),
            link,
            ..SpaceAbout::default()
        })
    }

    /// Deletes a non-General Teams channel where allowed and removes it from the local store.
    pub(super) fn delete_channel(&self, conversation: &str) -> Result<(), Fault> {
        if super::chain_of(conversation).is_some() {
            return Err(Fault::Refused {
                status: 400,
                message: "Channel reply threads cannot be deleted here.".to_owned(),
            });
        }
        let thread = thread_of(conversation);
        let Some(row) = self.ctx.known(thread)? else {
            return Err(Fault::Refused {
                status: 404,
                message: "This channel is gone.".to_owned(),
            });
        };
        if row.kind != ConversationKind::Channel {
            return Err(Fault::Refused {
                status: 400,
                message: "Only team channels can be deleted here.".to_owned(),
            });
        }
        if row.title.eq_ignore_ascii_case("general") || row.space.as_deref() == Some(thread) {
            return Err(Fault::Refused {
                status: 400,
                message: "The General channel cannot be deleted.".to_owned(),
            });
        }
        let team_id = row.space.as_deref().unwrap_or(thread);
        let url = format!(
            "{}/beta/teams/{}/channels/{}",
            self.place()?.middle,
            escaped(team_id),
            escaped(thread)
        );
        self.call("DELETE", &url, super::Auth::Both(super::SPACES), Body::None, false)
            .map_err(|fault| match fault {
                Fault::Refused { status: 403, .. } => Fault::Refused {
                    status: 403,
                    message: "Only team owners or members permitted by your organization's policy can delete this channel.".to_owned(),
                },
                other => other,
            })?;
        self.ctx.conversation_gone(thread)?;
        Ok(())
    }

    /// Updates Teams per-conversation notification level (`All`, `Mentions`, `Nothing`).
    pub(super) fn set_mute(&self, conversation: &str, notify: Notify) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let channel = super::tree::is_channel(thread);
        let path = format!("/v1/users/ME/conversations/{}/properties?name=alerts", escaped(thread));
        let alerts_body = match notify {
            Notify::All => json!({"alerts": "true"}),
            Notify::Mentions => json!({"alerts": "false", "mentions": "true"}),
            Notify::Nothing => json!({"alerts": "false"}),
        };
        self.chat("PUT", &path, Body::Json(&alerts_body), false)?;
        if channel {
            let settings_path = format!(
                "/v1/users/ME/conversations/{}/properties?name=channelNotificationSettings",
                escaped(thread)
            );
            let settings_body = match notify {
                Notify::All => json!({"channelNotificationSettings": {"allPosts": "bannerAndFeed", "mentions": "bannerAndFeed"}}),
                Notify::Mentions => json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "bannerAndFeed"}}),
                Notify::Nothing => json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "off"}}),
            };
            let _ = self.chat("PUT", &settings_path, Body::Json(&settings_body), false);
        }
        self.state().notify.insert(thread.to_owned(), notify);
        self.ctx.set_note(&format!("notify:{thread}"), notify_word(notify));
        self.ctx.muted(thread, notify == Notify::Nothing)?;
        Ok(())
    }

    /// Leaves a group chat by removing myself from its member list, then drops the row.
    pub(super) fn leave(&self, conversation: &str) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let path = format!("/v1/threads/{}/members/{}", escaped(thread), escaped(&self.me()));
        self.chat("DELETE", &path, Body::None, false)?;
        self.ctx.conversation_gone(conversation)?;
        Ok(())
    }

    /// Pins/unpins a Teams chat or channel, and shows/hides a Teams channel (`isFavorite` /
    /// `channelSettings`), updating local `Filing` and `opted_out` cleanly.
    pub(super) fn file_conversation(&self, conversation: &str, pinned: Option<bool>, archived: Option<bool>) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "System streams cannot be filed here.".to_owned(),
            });
        }
        if super::tree::is_channel(thread) {
            if let Some(pin) = pinned {
                let path = format!("/v1/users/ME/conversations/{}/properties?name=pinned", escaped(thread));
                self.chat("PUT", &path, Body::Json(&json!({"pinned": if pin { "true" } else { "false" }})), false)?;
                let pin_pos = {
                    let mut state = self.state();
                    if pin {
                        if let Some(pos) = state.pinned_channels.iter().position(|id| id == thread) {
                            Some(pos as i32)
                        } else {
                            state.pinned_channels.push(thread.to_owned());
                            Some((state.pinned_channels.len() - 1) as i32)
                        }
                    } else {
                        state.pinned_channels.retain(|id| id != thread);
                        None
                    }
                };
                let existing = self.ctx.known(thread)?;
                let filing = crate::store::Filing {
                    hidden: existing.as_ref().is_some_and(|row| row.hidden),
                    pin: pin_pos,
                    ..crate::store::Filing::default()
                };
                self.ctx.filing(thread, &filing)?;
            }
            if let Some(hide) = archived {
                let path = format!("/v1/users/ME/conversations/{}/properties?name=channelSettings", escaped(thread));
                let body = json!({"channelSettings": {"isFavorite": !hide}});
                self.chat("PUT", &path, Body::Json(&body), false)?;
                if hide {
                    self.state().pinned_channels.retain(|id| id != thread);
                }
                if let Some(existing) = self.ctx.known(thread)? {
                    let pin = if hide { None } else { existing.pin };
                    let mut row = crate::store::ConversationRow::new(thread, existing.kind, existing.title);
                    row.space = existing.space;
                    row.group = existing.group;
                    row.position = existing.position;
                    row.call = existing.call;
                    row.private = existing.private;
                    row.shared = existing.shared;
                    row.mark = existing.mark;
                    row.read_only = existing.read_only;
                    row.opted_out = hide;
                    row.filing = Some(crate::store::Filing {
                        hidden: existing.hidden,
                        pin,
                        ..crate::store::Filing::default()
                    });
                    self.ctx.conversation(&row)?;
                }
            }
            return Ok(());
        }
        let Some(pin) = pinned else {
            return Ok(());
        };
        if pin {
            let state = self.state();
            if !state.pinned_chats.iter().any(|id| id == thread) && state.pinned_chats.len() >= 15 {
                return Err(Fault::Refused {
                    status: 400,
                    message: "Teams allows at most 15 pinned chats.".to_owned(),
                });
            }
        }
        let path = format!("/v1/users/ME/conversations/{}/properties?name=pinned", escaped(thread));
        self.chat("PUT", &path, Body::Json(&json!({"pinned": if pin { "true" } else { "false" }})), false)?;
        let pin_pos = {
            let mut state = self.state();
            if pin {
                if let Some(pos) = state.pinned_chats.iter().position(|id| id == thread) {
                    Some(pos as i32)
                } else {
                    state.pinned_chats.push(thread.to_owned());
                    Some((state.pinned_chats.len() - 1) as i32)
                }
            } else {
                state.pinned_chats.retain(|id| id != thread);
                None
            }
        };
        let existing = self.ctx.known(thread)?;
        let filing = crate::store::Filing {
            hidden: existing.as_ref().is_some_and(|row| row.hidden),
            pin: pin_pos,
            ..crate::store::Filing::default()
        };
        self.ctx.filing(thread, &filing)?;
        Ok(())
    }

    /// Hides a Teams chat (`properties?name=hidden`) until a new message arrives.
    pub(super) fn hide_chat(&self, conversation: &str) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Only Teams chats can be hidden here.".to_owned(),
            });
        }
        let path = format!("/v1/users/ME/conversations/{}/properties?name=hidden", escaped(thread));
        self.chat("PUT", &path, Body::Json(&json!({"hidden": "true"})), false)?;
        let existing = self.ctx.known(thread)?;
        let filing = crate::store::Filing {
            hidden: true,
            pin: existing.as_ref().and_then(|row| row.pin),
            ..crate::store::Filing::default()
        };
        self.ctx.filing(thread, &filing)?;
        Ok(())
    }

    /// Deletes a Teams chat for the current user only and removes it from the local store.
    pub(super) fn delete_chat(&self, conversation: &str, delete_chat: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Teams channels cannot be deleted here.".to_owned(),
            });
        }
        if !delete_chat {
            return Err(Fault::Refused {
                status: 400,
                message: "Teams only supports deleting the entire chat for yourself.".to_owned(),
            });
        }
        let path = format!("/v1/users/ME/conversations/{}", escaped(thread));
        self.chat("DELETE", &path, Body::None, false)?;
        self.state().pinned_chats.retain(|id| id != thread);
        self.ctx.conversation_gone(thread)?;
        Ok(())
    }

    /// Accepts or declines/blocks a pending external Teams chat request (`Special::Request`).
    pub(super) fn answer_external_request(&self, conversation: &str, accept: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let known = self.ctx.known(thread)?;
        let path = format!("/v1/users/ME/conversations/{}/properties?name=externalChatState", escaped(thread));
        if accept {
            self.chat("PUT", &path, Body::Json(&json!({"externalChatState": "accepted"})), false)?;
            if let Some(known) = known {
                let row = crate::store::ConversationRow {
                    space: known.space,
                    group: known.group,
                    position: known.position,
                    avatar: known.avatar.map(|a| a.id),
                    peer: known.peer.clone(),
                    call: known.call,
                    call_video: known.call_video,
                    limit: known.limit,
                    topic: known.topic,
                    parent: known.parent,
                    root: known.root,
                    read_only: known.read_only,
                    read_only_reason: known.read_only_reason,
                    private: known.private,
                    encryption: known.encryption,
                    bot: known.bot,
                    meeting: known.meeting,
                    special: protocol::Special::None,
                    mark: known.mark,
                    opted_out: known.opted_out,
                    rights: known.rights,
                    can_pin: known.can_pin,
                    moderate: known.moderate,
                    joined: known.joined,
                    ..crate::store::ConversationRow::new(&known.key.id, known.kind, &known.title)
                };
                self.ctx.conversation(&row)?;
                if let Some(peer) = known.peer.filter(|p| p.starts_with("8:")) {
                    self.state().partners.insert(peer.clone(), thread.to_owned());
                    self.ctx.relation(&peer, protocol::Relation::Contact)?;
                }
            }
            Ok(())
        } else {
            self.chat("PUT", &path, Body::Json(&json!({"externalChatState": "blocked"})), false)?;
            if let Some(peer) = known.and_then(|k| k.peer).filter(|p| p.starts_with("8:")) {
                self.relate(&peer, protocol::RelateAction::Block)?;
            }
            self.ctx.conversation_gone(thread)?;
            Ok(())
        }
    }

    /// Renames a Teams group chat or channel and/or updates its description.
    pub(super) fn update_conversation(
        &self,
        conversation: &str,
        title: Option<&str>,
        topic: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), Fault> {
        if super::chain_of(conversation).is_some() {
            return Err(Fault::Refused {
                status: 400,
                message: "Reply threads cannot be renamed or edited here.".to_owned(),
            });
        }
        let thread = thread_of(conversation);
        let row = self.ctx.known(thread)?;
        let is_one_on_one = thread.ends_with("@unq.gbl.spaces")
            || row.as_ref().is_some_and(|r| r.kind == ConversationKind::Direct);
        if is_one_on_one {
            return Err(Fault::Refused {
                status: 400,
                message: "One-to-one Teams chats cannot be renamed.".to_owned(),
            });
        }
        let is_channel = super::tree::is_channel(thread);
        if let Some(name) = title.map(str::trim).filter(|s| !s.is_empty()) {
            if is_channel
                && row
                    .as_ref()
                    .is_some_and(|r| r.title.eq_ignore_ascii_case("general") || r.space.as_deref() == Some(thread))
            {
                return Err(Fault::Refused {
                    status: 400,
                    message: "The General channel cannot be renamed.".to_owned(),
                });
            }
            let path = format!("/v1/threads/{}/properties?name=topic", escaped(thread));
            self.chat("PUT", &path, Body::Json(&json!({"topic": name})), false)?;
            self.rename_row(thread, name)?;
        }
        if let Some(desc) = description.or(topic).map(str::trim) {
            let path = format!("/v1/threads/{}/properties?name=description", escaped(thread));
            self.chat("PUT", &path, Body::Json(&json!({"description": desc})), false)?;
        }
        self.ctx.info_stale(thread);
        Ok(())
    }

    /// Updates a stored conversation's title in place while preserving its metadata and rights.
    pub(super) fn rename_row(&self, thread: &str, new_title: &str) -> Result<(), Fault> {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let Some(known) = self.ctx.known(thread)? else {
            return Ok(());
        };
        if known.title == trimmed {
            return Ok(());
        }
        let row = crate::store::ConversationRow {
            space: known.space,
            group: known.group,
            position: known.position,
            avatar: known.avatar.map(|a| a.id),
            peer: known.peer,
            call: known.call,
            call_video: known.call_video,
            limit: known.limit,
            topic: known.topic,
            parent: known.parent,
            root: known.root,
            read_only: known.read_only,
            read_only_reason: known.read_only_reason,
            private: known.private,
            encryption: known.encryption,
            bot: known.bot,
            meeting: known.meeting,
            special: known.special,
            mark: known.mark,
            opted_out: known.opted_out,
            rights: known.rights,
            can_pin: known.can_pin,
            moderate: known.moderate,
            joined: known.joined,
            ..crate::store::ConversationRow::new(&known.key.id, known.kind, trimmed)
        };
        self.ctx.conversation(&row)?;
        Ok(())
    }

    /// After a chat's newest page: when my message is the newest, whether everyone else has
    /// read up to it, from their read markers. Not for channels, nor for large chats.
    pub(super) fn seen_by(&self, conversation: &str) -> Result<(), Fault> {
        let Some(row) = self.ctx.known(conversation)? else {
            return Ok(());
        };
        if row.kind == ConversationKind::Channel || row.kind == ConversationKind::Thread || conversation.starts_with("48:") || !row.preview_mine {
            return Ok(());
        }
        let path = format!("/v1/threads/{}/consumptionhorizons", escaped(conversation));
        let answer = match self.chat("GET", &path, Body::None, true) {
            Ok(answer) => answer.body,
            // A tenant or a chat that does not say: nothing is claimed.
            Err(Fault::Refused { .. }) => return Ok(()),
            Err(fault) => return Err(fault),
        };
        let me = self.me();
        let entries: Vec<(String, i64)> = answer["consumptionhorizons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|member| {
                let id = member["id"].as_str()?;
                if id.is_empty() || id == me {
                    return None;
                }
                let arrival = member["consumptionhorizon"].as_str()?.split(';').next().map(millis).unwrap_or(0);
                Some((id.to_owned(), arrival))
            })
            .collect();
        if entries.is_empty() || entries.len() > SMALL {
            return Ok(());
        }
        {
            let mut state = self.state();
            let map = state.member_horizons.entry(conversation.to_owned()).or_default();
            for (id, arrival) in &entries {
                if *arrival > 0 {
                    map.insert(id.clone(), *arrival);
                }
            }
        }
        // Everyone has read up to the least of their markers.
        if let Some(least) = entries.into_iter().map(|(_, ms)| ms).min().filter(|least| *least > 0) {
            self.ctx.seen(conversation, &least.to_string())?;
        }
        Ok(())
    }

    /// Resolves a pinned entry's `pinned_by` field (turning an `8:` MRI into a display name).
    fn pinned_by_name(&self, raw: Option<&str>) -> Option<String> {
        let who = raw?.trim();
        if who.is_empty() {
            return None;
        }
        if who.starts_with("8:") || who.starts_with("28:") {
            self.name_of(who).or_else(|| Some(who.to_owned()))
        } else {
            Some(who.to_owned())
        }
    }

    /// Looks up a single message from local store first, then falls back to fetching it from the
    /// Teams chat service (`GET /v1/users/ME/conversations/{thread}/messages/{id}`).
    fn fetch_message(&self, conversation: &str, message_id: &str) -> Result<Option<Message>, Fault> {
        if let Some(stored) = self.ctx.stored_message(conversation, message_id)? {
            return Ok(Some(stored));
        }
        let thread = thread_of(conversation);
        if thread != conversation {
            if let Some(stored) = self.ctx.stored_message(thread, message_id)? {
                return Ok(Some(stored));
            }
        }
        let path = format!(
            "/v1/users/ME/conversations/{}/messages/{}?view=msnp24Equivalent",
            escaped(thread),
            escaped(message_id)
        );
        match self.chat("GET", &path, Body::None, true) {
            Ok(answer) => Ok(self.read_message(thread, &answer.body)),
            Err(Fault::Refused { status: 404, .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Reads the pinned messages of a chat or channel from the thread's `pinnedMessages` property,
    /// marks each message `pinned` in the store, and returns them newest first (up to `PINNED_LIMIT`).
    pub(super) fn pinned_messages(&self, conversation: &str) -> Result<Vec<Message>, Fault> {
        let thread = thread_of(conversation);
        if thread.starts_with("48:") {
            return Ok(Vec::new());
        }
        let described = self.described(thread)?;
        let entries = parse_pinned_messages(&described["properties"]);
        self.apply_pinned_update(thread, entries.clone())?;

        let mris: Vec<String> = entries
            .iter()
            .filter_map(|e| e.pinned_by.as_deref())
            .filter(|who| who.starts_with("8:") || who.starts_with("28:"))
            .map(str::to_owned)
            .collect();
        if !mris.is_empty() {
            self.lookup(&mris);
        }

        let mut out = Vec::new();
        for entry in entries.iter().take(protocol::PINNED_LIMIT) {
            if let Ok(Some(mut msg)) = self.fetch_message(conversation, &entry.message_id) {
                msg.pinned = true;
                msg.pinned_by = self.pinned_by_name(entry.pinned_by.as_deref());
                msg.pinned_at = entry.pinned_at;
                out.push(msg);
            }
        }
        out.sort_by(|a, b| {
            let ta = a.pinned_at.unwrap_or(a.sent);
            let tb = b.pinned_at.unwrap_or(b.sent);
            tb.cmp(&ta).then_with(|| b.sent.cmp(&a.sent))
        });
        Ok(out)
    }

    /// Pins or unpins a message in a Teams chat or channel by updating the thread's
    /// `properties.pinnedMessages` list.
    pub(super) fn pin_message(&self, conversation: &str, message_id: &str, pinned: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Messages cannot be pinned in this conversation.".to_owned(),
            });
        }
        let described = self.described(thread)?;
        let mut entries = parse_pinned_messages(&described["properties"]);
        let me = self.me();
        let now_secs = now_ms() / 1000;
        if pinned {
            if !entries.iter().any(|e| e.message_id == message_id) {
                entries.push(PinnedEntry {
                    message_id: message_id.to_owned(),
                    pinned_by: (!me.is_empty()).then_some(me),
                    pinned_at: Some(now_secs),
                });
            }
        } else {
            entries.retain(|e| e.message_id != message_id);
        }
        let path = format!("/v1/threads/{}/properties?name=pinnedMessages", escaped(thread));
        let body = json!({"pinnedMessages": encode_pinned_messages(&entries)});
        self.chat("PUT", &path, Body::Json(&body), false)?;
        self.apply_pinned_update(thread, entries)?;
        self.ctx.pinned(conversation, message_id, pinned)?;
        if thread != conversation {
            self.ctx.pinned(thread, message_id, pinned)?;
        }
        self.ctx.info_stale(thread);
        Ok(())
    }

    /// Applies an updated list of pinned entries for `thread` (from `described()`, `pin_message()`,
    /// or a live `ConversationUpdate`/`ThreadUpdate` push) to the local cache and store.
    pub(super) fn apply_pinned_update(&self, thread: &str, entries: Vec<PinnedEntry>) -> Result<(), Fault> {
        let prev = self.state().pinned_messages.insert(thread.to_owned(), entries.clone()).unwrap_or_default();
        for old in &prev {
            if !entries.iter().any(|cur| cur.message_id == old.message_id) {
                let _ = self.ctx.pinned(thread, &old.message_id, false);
            }
        }
        for cur in &entries {
            let _ = self.ctx.pinned(thread, &cur.message_id, true);
        }
        Ok(())
    }

    /// Saves or unsaves a message in the user's personal Teams Saved messages collection (`48:saved`).
    pub(super) fn save_message(&self, conversation: &str, message_id: &str, starred: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let path = format!("/v1/users/ME/conversations/{}/messages/{}", escaped(SAVED_STREAM), escaped(message_id));
        if starred {
            let body = json!({
                "sourceConversationId": thread,
                "sourceMessageId": message_id,
                "savedTime": now_ms(),
            });
            self.chat("PUT", &path, Body::Json(&body), false)?;
            self.state().saved_ids.insert(message_id.to_owned());
        } else {
            self.chat("DELETE", &path, Body::None, false)?;
            self.state().saved_ids.remove(message_id);
        }
        if let Ok(Some(mut stored)) = self.ctx.stored_message(conversation, message_id) {
            stored.starred = starred;
            self.ctx.history(conversation, &[stored])?;
        }
        if thread != conversation {
            if let Ok(Some(mut stored)) = self.ctx.stored_message(thread, message_id) {
                stored.starred = starred;
                self.ctx.history(thread, &[stored])?;
            }
        }
        Ok(())
    }

    /// Lists saved messages from Teams' personal `48:saved` stream, newest first.
    pub(super) fn saved_messages(&self, conversation: Option<&str>, limit: u32) -> Result<Vec<Message>, Fault> {
        let count = limit.clamp(1, protocol::PAGE_LIMIT);
        let (items, cursor) = self.page(SAVED_STREAM, count, "startTime=1")?;
        let filter_thread = conversation.map(thread_of);
        let mut out = Vec::new();
        let mut saved_ids = Vec::new();
        for item in &items {
            let props = &item["properties"];
            let src_conv = [
                props["sourceConversationId"].as_str(),
                props["sourceThreadId"].as_str(),
                item["sourceConversationId"].as_str(),
                item["conversationid"].as_str(),
            ]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty() && *s != SAVED_STREAM)
            .unwrap_or("");
            let src_id = [
                &props["sourceMessageId"],
                &item["sourceMessageId"],
                &item["id"],
            ]
            .into_iter()
            .map(id_of)
            .find(|id| !id.is_empty())
            .unwrap_or_default();
            if src_id.is_empty() {
                continue;
            }
            saved_ids.push(src_id.clone());
            if let Some(want) = filter_thread {
                if src_conv.is_empty() || thread_of(src_conv) != want {
                    continue;
                }
            }
            let conv_target = if src_conv.is_empty() { SAVED_STREAM } else { src_conv };
            let resolved = if !item["content"].as_str().unwrap_or("").is_empty() {
                let mut msg = self.read_message(conv_target, item);
                if let Some(m) = msg.as_mut() {
                    m.id = src_id.clone();
                }
                msg
            } else if !src_conv.is_empty() {
                self.fetch_message(src_conv, &src_id).ok().flatten()
            } else {
                None
            };
            if let Some(mut msg) = resolved {
                msg.starred = true;
                out.push(msg);
            }
        }
        {
            let mut state = self.state();
            // One page is not the whole Later list. Replace the saved-id set only when
            // Teams says this page is the entire stream.
            let complete = cursor.is_empty() && (items.len() as u32) < count;
            if filter_thread.is_none() && complete {
                state.saved_ids = saved_ids.into_iter().collect();
            } else {
                for id in saved_ids {
                    state.saved_ids.insert(id);
                }
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.sent));
        out.truncate(count as usize);
        Ok(out)
    }

    /// Returns who has read `message_id` in a Teams chat (up to [`SMALL`] members).
    pub(super) fn read_by(&self, conversation: &str, message_id: &str) -> Result<Vec<protocol::ReadBy>, Fault> {
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || super::chain_of(conversation).is_some() || thread.starts_with("48:") {
            return Ok(Vec::new());
        }
        let target_ms = millis(message_id);
        let path = format!("/v1/threads/{}/consumptionhorizons", escaped(thread));
        let answer = match self.chat("GET", &path, Body::None, true) {
            Ok(answer) => answer.body,
            Err(Fault::Refused { .. }) => return Ok(Vec::new()),
            Err(fault) => return Err(fault),
        };
        let me = self.me();
        let mut parsed: Vec<(String, i64, i64)> = Vec::new();
        for member in answer["consumptionhorizons"].as_array().into_iter().flatten() {
            let Some(id) = member["id"].as_str().filter(|id| !id.is_empty() && *id != me) else {
                continue;
            };
            let Some(horizon) = member["consumptionhorizon"].as_str() else {
                continue;
            };
            let mut parts = horizon.split(';');
            let arrival = parts.next().map(millis).unwrap_or(0);
            let client_ts = parts.next().map(millis).unwrap_or(0);
            parsed.push((id.to_owned(), arrival, client_ts));
        }
        if parsed.is_empty() || parsed.len() > SMALL {
            return Ok(Vec::new());
        }
        {
            let mut state = self.state();
            let map = state.member_horizons.entry(thread.to_owned()).or_default();
            for (id, arrival, _) in &parsed {
                if *arrival > 0 {
                    map.insert(id.clone(), *arrival);
                }
            }
        }
        if let Some(least) = parsed.iter().map(|(_, ms, _)| *ms).min().filter(|least| *least > 0) {
            let _ = self.ctx.seen(thread, &least.to_string());
        }
        let matching: Vec<(String, i64, i64)> = parsed
            .into_iter()
            .filter(|(_, arrival, _)| *arrival > 0 && (target_ms == 0 || *arrival >= target_ms))
            .collect();
        let ids: Vec<String> = matching.iter().map(|(id, _, _)| id.clone()).collect();
        self.lookup(&ids);
        let readers = matching
            .into_iter()
            .map(|(id, arrival, client_ts)| {
                let name = self.name_of(&id).unwrap_or_default();
                let avatar = self.picture(&id, &name);
                let at = if client_ts > 0 { client_ts / 1000 } else { arrival / 1000 };
                protocol::ReadBy {
                    person: self.ctx.key(&id),
                    name,
                    avatar,
                    at,
                }
            })
            .collect();
        Ok(readers)
    }

    /// Updates live Seen state from an array of `{id, consumptionhorizon}` entries pushed by Teams.
    pub(super) fn apply_member_horizons(&self, conversation: &str, entries: &[Value]) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Ok(());
        }
        let me = self.me();
        let mut updated = false;
        let least = {
            let mut state = self.state();
            let map = state.member_horizons.entry(thread.to_owned()).or_default();
            for member in entries {
                let Some(id) = member["id"].as_str().filter(|id| !id.is_empty() && *id != me) else {
                    continue;
                };
                let arrival = member["consumptionhorizon"]
                    .as_str()
                    .and_then(|h| h.split(';').next())
                    .map(millis)
                    .unwrap_or(0);
                if arrival > 0 {
                    let prev = map.get(id).copied().unwrap_or(0);
                    if arrival >= prev {
                        map.insert(id.to_owned(), arrival);
                        updated = true;
                    }
                }
            }
            if updated && !map.is_empty() && map.len() <= SMALL {
                map.values().copied().min().filter(|ms| *ms > 0)
            } else {
                None
            }
        };
        if let Some(least) = least {
            self.ctx.seen(thread, &least.to_string())?;
        }
        Ok(())
    }

    /// Updates live Seen state when one member's consumption horizon arrives via push.
    pub(super) fn apply_single_member_horizon(&self, conversation: &str, member_id: &str, horizon: &str) -> Result<(), Fault> {
        if member_id.is_empty() || horizon.is_empty() {
            return Ok(());
        }
        let entry = json!({"id": member_id, "consumptionhorizon": horizon});
        self.apply_member_horizons(conversation, &[entry])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pinned_messages_handles_json_string_array_and_plain_ids() {
        let props = json!({
            "pinnedMessages": json!([
                {"messageId": "1760000100100", "pinnedBy": "8:orgid:ana", "pinnedTime": 1_760_000_200_000i64},
                {"id": "1760000100200", "pinnedByDisplayName": "Ben Okafor", "pinnedAt": "2025-10-09T09:05:00.0000000Z"},
                {"messageId": "1760000100100", "pinnedBy": "8:orgid:ana"}
            ]).to_string()
        });
        let parsed = parse_pinned_messages(&props);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].message_id, "1760000100100");
        assert_eq!(parsed[0].pinned_by.as_deref(), Some("8:orgid:ana"));
        assert_eq!(parsed[0].pinned_at, Some(1_760_000_200));
        assert_eq!(parsed[1].message_id, "1760000100200");
        assert_eq!(parsed[1].pinned_by.as_deref(), Some("Ben Okafor"));
        assert!(parsed[1].pinned_at.is_some());

        let ids_only = json!({"pinnedMessageIds": ["1760000100100", 1760000100300u64]});
        let parsed2 = parse_pinned_messages(&ids_only);
        assert_eq!(parsed2.len(), 2);
        assert_eq!(parsed2[0].message_id, "1760000100100");
        assert_eq!(parsed2[1].message_id, "1760000100300");

        let roundtrip = json!({"pinnedMessages": encode_pinned_messages(&parsed)});
        assert_eq!(parse_pinned_messages(&roundtrip), parsed);
    }

    #[test]
    fn parse_notify_reads_channel_settings_and_alerts_properties() {
        assert_eq!(
            parse_notify(&json!({"channelNotificationSettings": {"allPosts": "bannerAndFeed", "mentions": "bannerAndFeed"}}), true),
            Some(Notify::All)
        );
        assert_eq!(
            parse_notify(&json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "bannerAndFeed"}}), true),
            Some(Notify::Mentions)
        );
        assert_eq!(
            parse_notify(&json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "off"}}), true),
            Some(Notify::Nothing)
        );
        assert_eq!(
            parse_notify(&json!({"alerts": "false", "mentions": "true"}), false),
            Some(Notify::Mentions)
        );
        assert_eq!(parse_notify(&json!({"alerts": "false"}), false), Some(Notify::Nothing));
        assert_eq!(parse_notify(&json!({"alerts": "true"}), false), Some(Notify::All));
        assert_eq!(parse_notify(&json!({"isFollowed": true}), true), Some(Notify::All));
        assert_eq!(parse_notify(&json!({"isMuted": true}), true), Some(Notify::Nothing));
    }
}
