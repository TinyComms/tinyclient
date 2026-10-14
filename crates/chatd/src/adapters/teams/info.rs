//! A chat's or channel's info pane, its mute, and whether the other side has seen my last
//! message. Each is one request to the chat service, made because the user opened something.
//!
//! Unverified on a real tenant, most likely wrong first:
//! - Mute is the chat service's `alerts` property, as Skype's web client writes it; the Teams
//!   client may keep its own notification setting elsewhere (no client read sets it).
//! - "Seen" is read from `GET /v1/threads/{id}/consumptionhorizons`, the members' read markers.
//! - A thread's `createdat` and `creator` properties, read off the thread description.

use super::{escaped, id_of, millis, thread_of, Body, Fault, Teams};
use crate::adapters::ConversationAbout;
use protocol::{ConversationKind, Notify};
use serde_json::{json, Value};

/// "Seen" is told in one-to-one and small chats only, never in channels, as Teams does.
const SMALL: usize = 20;

impl Teams {
    fn described(&self, thread: &str) -> Result<Value, Fault> {
        Ok(self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true)?.body)
    }

    pub(super) fn conversation_info(&self, conversation: &str) -> Result<ConversationAbout, Fault> {
        let thread = thread_of(conversation);
        let row = self.ctx.known(thread)?;
        let notify = if row.as_ref().is_some_and(|row| row.muted) { Notify::Nothing } else { Notify::All };
        if thread.starts_with("48:") {
            return Ok(ConversationAbout { notify, ..ConversationAbout::default() });
        }
        let described = self.described(thread)?;
        let properties = &described["properties"];
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
        Ok(ConversationAbout {
            // A chat's topic is its title, already on the row: only a description is more.
            description: text("description"),
            members,
            created: (created > 0).then_some(created),
            creator,
            notify,
            external: if properties["sharedChannel"] == true || properties["threadType"] == "sharedChannel" { "Shared channel".to_owned() } else { String::new() },
            ..ConversationAbout::default()
        })
    }

    /// Teams has on and off for a chat: `Mentions` keeps the mention alerts, which a muted
    /// chat still raises in Teams, so it is off too.
    pub(super) fn set_mute(&self, conversation: &str, notify: Notify) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let muted = notify != Notify::All;
        let path = format!("/v1/users/ME/conversations/{}/properties?name=alerts", escaped(thread));
        self.chat("PUT", &path, Body::Json(&json!({"alerts": if muted { "false" } else { "true" }})), false)?;
        self.ctx.muted(thread, muted)?;
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
