//! A chat's or channel's info pane, its mute, and whether the other side has seen my last
//! message. Each is one request to the chat service, made because the user opened something.
//!
//! Unverified on a real tenant, most likely wrong first:
//! - Mute is the chat service's `alerts` property, as Skype's web client writes it; the Teams
//!   client may keep its own notification setting elsewhere (no client read sets it).
//! - "Seen" is read from `GET /v1/threads/{id}/consumptionhorizons`, the members' read markers.
//! - A thread's `createdat` and `creator` properties, read off the thread description.

use super::links;
use super::{escaped, id_of, millis, thread_of, Body, Fault, Teams};
use crate::adapters::{ConversationAbout, SpaceAbout};
use protocol::{ConversationKind, Notify};
use serde_json::{json, Value};

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
        } else if channel {
            Notify::Mentions
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
        let shared = properties["sharedChannel"] == true || properties["threadType"] == "sharedChannel";
        Ok(ConversationAbout {
            // A chat's topic is its title, already on the row: only a description is more.
            description: text("description"),
            members,
            created: (created > 0).then_some(created),
            creator,
            notify,
            external: if shared { "Shared channel".to_owned() } else { String::new() },
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
        Ok(SpaceAbout {
            description: text("description"),
            members,
            roles: role.map(str::to_owned).into_iter().collect(),
            ..SpaceAbout::default()
        })
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

    /// Pins or unpins a Teams chat and updates local `Filing::pin`.
    pub(super) fn file_chat(&self, conversation: &str, pinned: Option<bool>) -> Result<(), Fault> {
        let Some(pin) = pinned else {
            return Ok(());
        };
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Only Teams chats can be pinned here.".to_owned(),
            });
        }
        let path = format!("/v1/users/ME/conversations/{}/properties?name=pinned", escaped(thread));
        self.chat("PUT", &path, Body::Json(&json!({"pinned": if pin { "true" } else { "false" }})), false)?;
        let pin_pos = {
            let mut state = self.state();
            if pin {
                if let Some(pos) = state.pinned_chats.iter().position(|id| id == thread) {
                    Some(pos as i32)
                } else {
                    if state.pinned_chats.len() >= 15 {
                        return Err(Fault::Refused {
                            status: 400,
                            message: "Teams allows at most 15 pinned chats.".to_owned(),
                        });
                    }
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
        let horizons: Vec<i64> = answer["consumptionhorizons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|member| member["id"].as_str().is_some_and(|id| id != me))
            .filter_map(|member| member["consumptionhorizon"].as_str()?.split(';').next().map(millis))
            .collect();
        if horizons.is_empty() || horizons.len() > SMALL {
            return Ok(());
        }
        // Everyone has read up to the least of their markers.
        if let Some(least) = horizons.into_iter().min().filter(|least| *least > 0) {
            self.ctx.seen(conversation, &least.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
