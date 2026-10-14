//! Teams as Spaces: each team with its channels as the tree, the chat list as a Space of its
//! own, and the unread state Teams keeps for all of it. Everything comes from one call.

use super::{chain_id, escaped, id_of, millis, seconds, thread_of, Auth, Body, Fault, Teams, AGGREGATOR, TREE_FRESH};
use crate::adapters::Command;
use crate::store::{ConversationRow, SpaceRow};
use protocol::{CallSupport, ConversationKind, Group, GroupKind, Relation};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

/// The Space, and the group in it, that hold the chat list.
/// CONTRACT: a conversation is in one Space, so the chats cannot be a `Directs` group in every
/// team's tree. They are a Space of their own, first, with that one group.
const CHATS: &str = "chats";
/// Unread chats whose newest page is fetched after connecting, paced, so their rows have a
/// preview. Everything else is fetched when it is opened.
const WARMED: usize = 5;
/// People looked up in one listing, for the names of chats.
const NAMED: usize = 200;
/// A row recorded outside a listing sorts last until the next one.
const LAST: i32 = i32::MAX;

fn taken(listed: &mut Value, name: &str) -> Vec<Value> {
    match listed.get_mut(name).map(Value::take) {
        Some(Value::Array(found)) => found,
        _ => Vec::new(),
    }
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().unwrap_or("")
}

fn is_channel(id: &str) -> bool {
    id.contains("@thread.tacv2") || id.contains("@thread.skype")
}

/// What Teams says of how far a channel or chat was read.
struct Counted {
    unread: bool,
    last_read: Option<String>,
    /// Unix seconds of the newest message.
    updated: i64,
}

/// A channel's or chat's read state: its newest message against the read marker where both are
/// told, else Teams' own flag. Unverified which of them a real account carries for a chat.
fn counted(entry: &Value, me: &str) -> Counted {
    let last = &entry["lastMessage"];
    let newest = id_of(&last["id"]);
    let read = id_of(&entry["consumptionHorizon"]["originalArrivalTime"]);
    let from = text(last, "from");
    let mine = entry["isLastMessageFromMe"] == true || (!me.is_empty() && from.rsplit('/').next() == Some(me));
    let unread = if mine || millis(&newest) == 0 {
        false
    } else if millis(&read) > 0 {
        millis(&newest) > millis(&read)
    } else {
        entry["isRead"] == false || entry["isMessageRead"] == false
    };
    Counted {
        unread,
        last_read: Some(read).filter(|read| millis(read) > 0),
        updated: match millis(&newest) / 1000 {
            0 => seconds(text(last, "composeTime")),
            updated => updated,
        },
    }
}

impl Teams {
    /// The shell opened a Space, or push has just connected.
    pub(super) fn open_space(&self) -> Result<(), Fault> {
        let (again, listed) = {
            let mut state = self.state();
            (std::mem::take(&mut state.resync), state.listed)
        };
        match listed {
            Some(at) if !again && at.elapsed() < TREE_FRESH => Ok(()),
            listed => self.list(listed.is_none()),
        }
    }

    /// Every team with its channels, and every chat, from the one call Teams' own client makes
    /// for its sidebar. Rows go to the store team by team and nothing is kept.
    fn list(&self, first: bool) -> Result<(), Fault> {
        let url = format!("{}/api/csa/api/v1/teams/users/me?isPrefetch=false&enableMembershipSummary=true", self.api);
        let mut listed = self.call("GET", &url, Auth::Both(AGGREGATOR), Body::None, true)?.body;
        let (teams, chats) = (taken(&mut listed, "teams"), taken(&mut listed, "chats"));
        drop(listed);
        let me = self.me();
        let teams: Vec<&Value> = teams.iter().filter(|team| team["isDeleted"] != true && !text(team, "id").is_empty()).collect();
        let mut spaces = vec![SpaceRow {
            id: CHATS.to_owned(),
            name: "Chats".to_owned(),
            icon: None,
            position: 0,
        }];
        spaces.extend(teams.iter().enumerate().map(|(at, team)| SpaceRow {
            id: text(team, "id").to_owned(),
            name: text(team, "displayName").to_owned(),
            icon: None,
            position: at as i32 + 1,
        }));
        self.ctx.spaces(&spaces)?;

        for team in teams {
            let id = text(team, "id");
            // General first, then the order Teams lists them in. A channel the user hid is left
            // out. Unverified: that `isFavorite: false` is what hiding a channel writes.
            let shown = |channel: &&Value| {
                channel["isDeleted"] != true && (channel["isGeneral"] == true || channel["isFavorite"] != false || channel["isPinned"] == true)
            };
            let mut channels: Vec<&Value> = team["channels"].as_array().into_iter().flatten().filter(shown).collect();
            channels.sort_by_key(|channel| channel["isGeneral"] != true);
            let rows: Vec<ConversationRow> = channels
                .iter()
                .enumerate()
                .map(|(position, channel)| ConversationRow {
                    space: Some(id.to_owned()),
                    position: position as i32,
                    call: CallSupport::Handoff,
                    private: channel["channelType"].as_u64().is_some_and(|kind| kind != 0),
                    read_only: team["isArchived"] == true || channel["isArchived"] == true,
                    ..ConversationRow::new(text(channel, "id"), ConversationKind::Channel, text(channel, "displayName"))
                })
                .collect();
            self.ctx.tree(id, &[], &rows)?;
            // The group behind the team, which a link into the official app has to name.
            if let Some(group) = team["teamSiteInformation"]["groupId"].as_str() {
                self.ctx.set_note(&format!("group:{id}"), group);
            }
            for channel in channels {
                self.count(text(channel, "id"), &counted(channel, &me), channel["isMuted"] == true)?;
            }
        }
        self.chats(chats, &me, first)?;
        self.state().listed = Some(Instant::now());
        Ok(())
    }

    /// Records Teams' read state for a row. Teams does not say how many messages are unread or
    /// name us, so "some" is one; what push counted since the row was last read is kept.
    fn count(&self, id: &str, counted: &Counted, muted: bool) -> Result<(), Fault> {
        let (mut unread, mut mentions) = (u32::from(counted.unread), 0);
        if let Some(before) = self.ctx.known(id)?.filter(|before| counted.unread && before.last_read == counted.last_read) {
            unread = unread.max(before.unread);
            mentions = before.mentions;
        }
        self.ctx.counts(id, unread, mentions, counted.last_read.as_deref(), counted.updated)?;
        self.ctx.muted(id, muted)?;
        Ok(())
    }

    fn chats(&self, chats: Vec<Value>, me: &str, first: bool) -> Result<(), Fault> {
        let others = |chat: &Value| -> Vec<String> {
            let members = chat["members"].as_array().into_iter().flatten();
            members.filter_map(|member| member["mri"].as_str()).filter(|mri| *mri != me).map(str::to_owned).collect()
        };
        // System streams (notifications, call logs, notes to self) are not conversations.
        let shown = |chat: &&Value| chat["hidden"] != true && chat["isConversationDeleted"] != true && !text(chat, "id").starts_with("48:");
        let mut chats: Vec<(&Value, Counted)> = chats.iter().filter(shown).map(|chat| (chat, counted(chat, me))).collect();
        // Teams' own order: the chat that spoke last is first.
        chats.sort_by_key(|(_, counted)| std::cmp::Reverse(counted.updated));
        // The people a chat is named after: its partner, or the first few of a group with no
        // title. Asked for together, once, and only those not already known.
        let mut wanted: Vec<String> = Vec::new();
        for (chat, _) in &chats {
            if chat["isOneOnOne"] == true || text(chat, "title").is_empty() {
                wanted.extend(others(chat).into_iter().take(3));
            }
        }
        wanted.truncate(NAMED);
        self.lookup(&wanted);

        let (mut rows, mut partners, mut warm) = (Vec::with_capacity(chats.len()), HashMap::new(), Vec::new());
        for (position, (chat, counted)) in chats.iter().enumerate() {
            let (id, people) = (text(chat, "id"), others(chat));
            let partner = people.first().filter(|_| chat["isOneOnOne"] == true && people.len() == 1);
            let mut row = self.chat_row(id, text(chat, "title"), &people, partner.map(String::as_str));
            row.position = position as i32;
            if row.title.is_empty() {
                row.title = Some(text(&chat["lastMessage"], "imDisplayName")).filter(|name| !name.is_empty()).unwrap_or("Chat").to_owned();
            }
            if let Some(partner) = partner {
                partners.insert(partner.clone(), id.to_owned());
            }
            if counted.unread && warm.len() < WARMED {
                warm.push(id.to_owned());
            }
            rows.push(row);
        }
        let group = Group {
            kind: GroupKind::Directs,
            ..Group::new(CHATS, "Chats", 0)
        };
        self.ctx.tree(CHATS, &[group], &rows)?;
        drop(rows);
        for (chat, counted) in &chats {
            let mine = chat["members"].as_array().into_iter().flatten().find(|member| member["mri"] == me);
            self.count(text(chat, "id"), counted, mine.is_some_and(|mine| mine["isMuted"] == true))?;
        }
        // The list the user owns on Teams: the people they have a chat with.
        let relations: Vec<(String, Relation)> = partners.keys().map(|person| (person.clone(), Relation::Contact)).collect();
        self.ctx.relations(&relations)?;
        self.state().partners = partners;
        if first {
            for id in warm {
                let _ = self.ctx.commands.send(Command::Warm(id));
            }
        }
        Ok(())
    }

    /// A chat's row: named after its partner, its own title, or the first people in it.
    fn chat_row(&self, id: &str, title: &str, people: &[String], partner: Option<&str>) -> ConversationRow {
        let name = |person: &String| self.name_of(person);
        let (kind, title) = match partner {
            Some(partner) => (ConversationKind::Direct, self.name_of(partner).unwrap_or_default()),
            None if !title.is_empty() => (ConversationKind::Group, title.to_owned()),
            None => {
                let mut names: Vec<String> = people.iter().take(3).filter_map(name).collect();
                if people.len() > names.len() && !names.is_empty() {
                    names.push(format!("+{}", people.len() - names.len()));
                }
                (ConversationKind::Group, names.join(", "))
            }
        };
        ConversationRow {
            space: Some(CHATS.to_owned()),
            group: Some(CHATS.to_owned()),
            position: LAST,
            avatar: partner.and_then(|partner| self.picture(partner, &title)).map(|file| file.id),
            peer: partner.map(str::to_owned),
            call: CallSupport::Handoff,
            ..ConversationRow::new(id, kind, title)
        }
    }

    /// True once the conversation a message belongs to has a row. A chat never seen (someone
    /// has just started it) is described by Teams and recorded; a channel that is not in the
    /// tree is one the user hid or left, and stays out.
    pub(super) fn known(&self, conversation: &str) -> Result<bool, Fault> {
        let thread = thread_of(conversation);
        if self.ctx.known(thread)?.is_some() {
            return Ok(true);
        }
        if is_channel(thread) || thread.starts_with("48:") || !thread.starts_with("19:") {
            return Ok(false);
        }
        let me = self.me();
        let described = self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true);
        let described = match described {
            Ok(described) => described.body,
            Err(Fault::Refused { .. }) => Value::Null,
            Err(fault) => return Err(fault),
        };
        let members = described["members"].as_array().into_iter().flatten();
        let people: Vec<String> = members.filter_map(|member| member["id"].as_str()).filter(|id| *id != me).map(str::to_owned).collect();
        self.lookup(&people[..people.len().min(3)]);
        let properties = &described["properties"];
        let one = people.len() == 1 && (properties["uniquerosterthread"] == "true" || thread.ends_with("@unq.gbl.spaces"));
        let partner = people.first().filter(|_| one).map(String::as_str);
        let mut row = self.chat_row(thread, text(properties, "topic"), &people, partner);
        if row.title.is_empty() {
            row.title = "Chat".to_owned();
        }
        self.ctx.conversation(&row)?;
        if let Some(partner) = partner {
            self.state().partners.insert(partner.to_owned(), thread.to_owned());
            self.ctx.relation(partner, Relation::Contact)?;
        }
        Ok(true)
    }

    /// A reply chain's own conversation, so its replies have somewhere to be read. It names
    /// the channel it hangs off and the post that started it, so it stays through a listing
    /// and goes when its channel goes.
    pub(super) fn chain_known(&self, channel: &str, root: &str) -> Result<(), Fault> {
        let id = chain_id(channel, root);
        if self.ctx.known(&id)?.is_some() {
            return Ok(());
        }
        let parent = self.ctx.known(channel)?;
        let title = parent.as_ref().map_or_else(|| "Replies".to_owned(), |parent| format!("Replies in {}", parent.title));
        let mut row = ConversationRow::new(id, ConversationKind::Thread, title);
        row.parent = Some(channel.to_owned());
        row.root = Some(root.to_owned());
        if let Some(parent) = parent {
            row.private = parent.private;
            row.read_only = parent.read_only;
        }
        self.ctx.conversation(&row)?;
        Ok(())
    }

    /// Where the official app opens this conversation to call in it: a call to the person of a
    /// one-to-one chat, the meeting of a meeting chat, else the chat or channel itself, where
    /// its call button is. Microsoft's documented deep links; none was seen in a client's code.
    pub(super) fn call_link(&self, conversation: &str) -> String {
        let thread = thread_of(conversation);
        let row = self.ctx.known(thread).ok().flatten();
        let chat = || format!("msteams:/l/chat/{}/conversations", escaped(thread));
        match row {
            _ if thread.starts_with("19:meeting_") => format!("msteams:/l/meetup-join/{}/0", escaped(thread)),
            Some(row) if row.kind == ConversationKind::Channel => {
                let group = row.space.as_ref().and_then(|team| self.ctx.note(&format!("group:{team}"))).unwrap_or_default();
                format!(
                    "msteams:/l/channel/{}/{}?groupId={}&tenantId={}",
                    escaped(thread),
                    escaped(&row.title),
                    escaped(&group),
                    escaped(&self.session.tenant)
                )
            }
            Some(row) if row.kind == ConversationKind::Direct => {
                // The call link names people by sign-in name, which is known once looked up.
                let mail = row.peer.as_deref().and_then(|peer| self.cached(peer)).map(|known| known.mail).filter(|mail| !mail.is_empty());
                mail.map_or_else(chat, |mail| format!("msteams:/l/call/0/0?users={}", escaped(&mail)))
            }
            _ => chat(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unread_is_the_newest_message_against_the_read_marker() {
        let me = "8:orgid:me";
        let channel = |newest: u64, read: u64, from: &str| {
            json!({
                "consumptionHorizon": {"originalArrivalTime": read, "timeStamp": read + 5, "clientMessageId": "1"},
                "lastMessage": {"id": newest.to_string(), "from": from, "composeTime": "2026-10-09T08:56:40.0000000Z"},
                "isMessageRead": true,
            })
        };
        let behind = counted(&channel(1_760_000_300_300, 1_760_000_100_100, "8:orgid:ana"), me);
        assert!(behind.unread);
        assert_eq!((behind.last_read.as_deref(), behind.updated), (Some("1760000100100"), 1_760_000_300));
        assert!(!counted(&channel(1_760_000_300_300, 1_760_000_300_300, "8:orgid:ana"), me).unread);
        // What I said last is never unread, however old the marker.
        assert!(!counted(&channel(1_760_000_300_300, 1_760_000_100_100, "8:orgid:me"), me).unread);
        let link = "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/8:orgid:me";
        assert!(!counted(&channel(1_760_000_300_300, 1_760_000_100_100, link), me).unread);
        // A chat with no marker goes by Teams' own flag.
        let flagged = json!({"isRead": false, "lastMessage": {"id": "1760000300300", "from": "8:orgid:ana"}});
        assert!(counted(&flagged, me).unread && counted(&flagged, me).last_read.is_none());
        let read = json!({"isRead": true, "isLastMessageFromMe": false, "lastMessage": {"id": "1760000300300", "from": "8:orgid:ana"}});
        assert!(!counted(&read, me).unread);
        assert!(!counted(&json!({"isRead": false}), me).unread, "a chat with no message has nothing unread");
    }

    #[test]
    fn channels_are_told_from_chats_by_their_ids() {
        assert!(is_channel("19:abc@thread.tacv2") && is_channel("19:abc@thread.skype"));
        assert!(!is_channel("19:a_b@unq.gbl.spaces") && !is_channel("19:meeting_x@thread.v2"));
    }
}
