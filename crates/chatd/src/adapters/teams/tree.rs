//! Teams' lists: the chats, which belong to no Space (meeting chats, app chats and the user's
//! own Notes among them), each team as a Space with its channels as the tree, and the unread
//! state Teams keeps for all of it. Everything comes from one call, and the Activity stream is
//! read once at connect for the mentions that came while tinyclient was closed.

use super::{chain_id, escaped, id_of, millis, seconds, thread_of, Auth, Body, Fault, Teams, AGGREGATOR, TREE_FRESH};
use crate::adapters::Command;
use crate::store::{ConversationRow, Filing, PreviewLine, SpaceRow};
use protocol::{CallSupport, ConversationKind, Relation, Special};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

/// The Space an earlier build kept the chats in. Gone: chats belong to no Space.
const OLD_CHATS: &str = "chats";
/// The user's own notes, Teams' "(You)" chat.
const NOTES: &str = "48:notes";
/// Teams' Activity stream: mentions, replies and reactions, as messages of a conversation.
const ACTIVITY: &str = "48:notifications";
/// The Activity stream's page read at connect.
const ACTIVITY_PAGE: u32 = 50;
/// The longest a reply chain's title is: the start of its post.
const CHAIN_TITLE: usize = 40;
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
        // The chats first: written with no Space, they leave the one an earlier build kept them
        // in before that Space goes.
        self.chats(chats, &me, first)?;
        let teams: Vec<&Value> = teams.iter().filter(|team| team["isDeleted"] != true && !text(team, "id").is_empty()).collect();
        let spaces: Vec<SpaceRow> = teams
            .iter()
            .enumerate()
            .map(|(at, team)| SpaceRow {
                id: text(team, "id").to_owned(),
                name: text(team, "displayName").to_owned(),
                icon: None,
                position: at as i32,
            })
            .filter(|space| space.id != OLD_CHATS)
            .collect();
        self.ctx.spaces(&spaces)?;

        for team in teams {
            let id = text(team, "id");
            // General first, then the order Teams lists them in. A channel the user hid is kept
            // apart (`opted_out`), behind the tree's "Show hidden". Unverified: that
            // `isFavorite: false` is what hiding a channel writes.
            let hidden = |channel: &Value| channel["isGeneral"] != true && channel["isFavorite"] == false && channel["isPinned"] != true;
            let mut channels: Vec<&Value> = team["channels"].as_array().into_iter().flatten().filter(|channel| channel["isDeleted"] != true).collect();
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
                    opted_out: hidden(channel),
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
        self.state().listed = Some(Instant::now());
        if first {
            if let Err(fault) = self.activity_feed() {
                tracing::warn!("chatd: Teams: activity: {}", super::reason(fault));
            }
        }
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
        // System streams are not conversations, but for the user's own notes. The Activity
        // stream is read for its mentions (`activity_feed`); the call log is not read.
        let shown = |chat: &&Value| {
            let id = text(chat, "id");
            chat["isConversationDeleted"] != true && !id.is_empty() && (!id.starts_with("48:") || id == NOTES)
        };
        let mut chats: Vec<(&Value, Counted)> = chats.iter().filter(shown).map(|chat| (chat, counted(chat, me))).collect();
        // Teams' own order: the chat that spoke last is first.
        chats.sort_by_key(|(_, counted)| std::cmp::Reverse(counted.updated));
        // The people a chat is named after: its partner (a person or an app), or the first few
        // of a group with no title. Asked for together, once, and only those not already known.
        let mut wanted: Vec<String> = Vec::new();
        for (chat, _) in &chats {
            if chat["isOneOnOne"] == true || text(chat, "title").is_empty() {
                wanted.extend(others(chat).into_iter().take(3));
            }
        }
        wanted.truncate(NAMED);
        self.lookup(&wanted);

        let (mut partners, mut warm) = (HashMap::new(), Vec::new());
        for (position, (chat, counted)) in chats.iter().enumerate() {
            let (id, people) = (text(chat, "id"), others(chat));
            // A one-to-one with a person, or with an app (`28:`), which Teams may not flag as
            // one-to-one (unverified).
            let alone = people.len() == 1 && (chat["isOneOnOne"] == true || people[0].starts_with("28:"));
            let partner = people.first().filter(|_| alone);
            let mut row = self.chat_row(id, &title_of(chat), &people, partner.map(String::as_str));
            row.position = position as i32;
            row.meeting = is_meeting(chat);
            row.filing = Some(Filing { hidden: chat["hidden"] == true, ..Filing::default() });
            if row.title.is_empty() {
                // Never the user's own name: the last sender is used only when it was not me.
                let last = &chat["lastMessage"];
                let from = text(last, "from");
                let other = !from.is_empty() && from.rsplit('/').next() != Some(me);
                row.title = match (other, last["imDisplayName"].as_str()) {
                    (true, Some(name)) if !name.is_empty() => name.to_owned(),
                    _ if people.len() > 1 => format!("Group chat ({})", people.len() + 1),
                    _ => "Chat".to_owned(),
                };
            }
            if let Some(partner) = partner.filter(|partner| partner.starts_with("8:")) {
                partners.insert(partner.clone(), id.to_owned());
            }
            if counted.unread && warm.len() < WARMED {
                warm.push(id.to_owned());
            }
            self.ctx.conversation(&row)?;
            self.count(id, counted, chat["members"].as_array().into_iter().flatten().any(|member| member["mri"] == me && member["isMuted"] == true))?;
            if let Some(line) = preview_of(chat, me) {
                self.ctx.preview(id, &line)?;
            }
        }
        // The list the user owns on Teams: the people they have a chat with. Apps are not people.
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

    /// A chat's row: named after its partner, its own title, or the first people in it. The
    /// user's Notes are `Saved`; a chat with an app is `bot` and named after the app.
    fn chat_row(&self, id: &str, title: &str, people: &[String], partner: Option<&str>) -> ConversationRow {
        let name = |person: &String| self.name_of(person);
        let me = self.me();
        let (kind, title) = match partner {
            _ if id == NOTES => (ConversationKind::Direct, "Notes".to_owned()),
            Some(partner) => (ConversationKind::Direct, self.name_of(partner).unwrap_or_default()),
            None if !title.is_empty() => (ConversationKind::Group, title.to_owned()),
            None => {
                // The first two, then how many more: "Ana, Ben, +3".
                let mut names: Vec<String> = people.iter().filter(|person| **person != me).take(2).filter_map(name).collect();
                if people.len() > names.len() && !names.is_empty() {
                    names.push(format!("+{}", people.len() - names.len()));
                }
                (ConversationKind::Group, names.join(", "))
            }
        };
        let bot = partner.is_some_and(|partner| partner.starts_with("28:"));
        let notes = id == NOTES;
        let avatar = match notes {
            true => self.picture(&me, &title),
            false => partner.and_then(|partner| self.picture(partner, &title)),
        };
        ConversationRow {
            avatar: avatar.map(|file| file.id),
            peer: partner.filter(|_| !bot).map(str::to_owned),
            call: if bot || notes { CallSupport::None } else { CallSupport::Handoff },
            bot,
            special: if notes { Special::Saved } else { Special::None },
            ..ConversationRow::new(id, kind, title)
        }
    }

    /// The Activity stream's newest page, once at connect: each mention of me newer than its
    /// conversation's read marker counts, so mentions that came while tinyclient was closed are
    /// not lost. Unverified: the shape of an activity (`properties.activity`), read off the web
    /// client's own reading of this stream.
    fn activity_feed(&self) -> Result<(), Fault> {
        let (items, _) = self.page(ACTIVITY, ACTIVITY_PAGE, "startTime=1")?;
        let mut found: HashMap<String, Vec<String>> = HashMap::new();
        for item in &items {
            let activity = match &item["properties"]["activity"] {
                Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
                other => other.clone(),
            };
            let kind = text(&activity, "activityType").to_ascii_lowercase();
            if !kind.contains("mention") {
                continue;
            }
            let (thread, message) = (text(&activity, "sourceThreadId"), id_of(&activity["sourceMessageId"]));
            if thread.is_empty() || millis(&message) == 0 {
                continue;
            }
            // A mention in a reply chain is the chain's.
            let chain = id_of(&activity["sourceReplyChainId"]);
            let place = if millis(&chain) > 0 && chain != message { chain_id(thread, &chain) } else { thread.to_owned() };
            let ids = found.entry(place).or_default();
            if !ids.contains(&message) {
                ids.push(message);
            }
        }
        for (place, messages) in found {
            let Some(row) = self.ctx.known(&place)?.or(self.ctx.known(thread_of(&place))?) else {
                continue;
            };
            let read = row.last_read.as_deref().map(millis).unwrap_or(0);
            let unseen = messages.iter().filter(|message| millis(message) > read).count() as u32;
            if unseen > row.mentions {
                let id = row.key.id.clone();
                let newest = messages.iter().map(|message| millis(message) / 1000).max().unwrap_or(0);
                self.ctx.counts(&id, row.unread.max(unseen), unseen, row.last_read.as_deref(), newest.max(row.updated))?;
            }
        }
        Ok(())
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
        let one = people.len() == 1 && (properties["uniquerosterthread"] == "true" || thread.ends_with("@unq.gbl.spaces") || people[0].starts_with("28:"));
        let partner = people.first().filter(|_| one).map(String::as_str);
        let mut row = self.chat_row(thread, text(properties, "topic"), &people, partner);
        row.meeting = thread.starts_with("19:meeting_");
        row.position = LAST;
        if row.title.is_empty() {
            row.title = if people.len() > 1 { format!("Group chat ({})", people.len() + 1) } else { "Chat".to_owned() };
        }
        self.ctx.conversation(&row)?;
        if let Some(partner) = partner.filter(|partner| partner.starts_with("8:")) {
            self.state().partners.insert(partner.to_owned(), thread.to_owned());
            self.ctx.relation(partner, Relation::Contact)?;
        }
        Ok(true)
    }

    /// A reply chain's own conversation, so its replies have somewhere to be read. It names
    /// the channel it hangs off and the post that started it, so it stays through a listing
    /// and goes when its channel goes.
    /// It is titled by its post: the subject, else the post's first words; "Replies" until
    /// the post is known, and retitled once it is.
    pub(super) fn chain_known(&self, channel: &str, root: &str) -> Result<(), Fault> {
        let id = chain_id(channel, root);
        let title = self.ctx.stored_message(channel, root)?.map(|post| chain_title(&post.text)).filter(|title| !title.is_empty());
        if let Some(held) = self.ctx.known(&id)? {
            if let Some(title) = title.filter(|title| *title != held.title) {
                let mut row = ConversationRow::new(id, ConversationKind::Thread, title);
                row.parent = Some(channel.to_owned());
                row.root = Some(root.to_owned());
                (row.private, row.read_only) = (held.private, held.read_only);
                self.ctx.conversation(&row)?;
            }
            return Ok(());
        }
        let parent = self.ctx.known(channel)?;
        let mut row = ConversationRow::new(id, ConversationKind::Thread, title.unwrap_or_else(|| "Replies".to_owned()));
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

/// A chat's own title: its topic, or a meeting's subject.
fn title_of(chat: &Value) -> String {
    let named = [&chat["title"], &chat["meetingInformation"]["subject"]];
    named.into_iter().filter_map(Value::as_str).map(str::trim).find(|title| !title.is_empty()).unwrap_or("").to_owned()
}

/// A meeting's chat: by its id, or by what the list says it is (unverified field).
fn is_meeting(chat: &Value) -> bool {
    text(chat, "id").starts_with("19:meeting_") || chat["chatType"] == "meeting" || chat["meetingInformation"].is_object()
}

/// The chat list's `lastMessage` as the row's preview line, its markup read for words alone.
fn preview_of(chat: &Value, me: &str) -> Option<PreviewLine> {
    let last = &chat["lastMessage"];
    let id = id_of(&last["id"]);
    let content = text(last, "content");
    if millis(&id) == 0 || content.is_empty() || !text(last, "messageType").starts_with("RichText") && text(last, "messageType") != "Text" {
        return None;
    }
    let words = super::html::parse(content, &super::people::Nobody, me).text;
    let words = words.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        return None;
    }
    let from = text(last, "from");
    Some(PreviewLine {
        id: id.clone(),
        text: words,
        sender: text(last, "imDisplayName").to_owned(),
        mine: chat["isLastMessageFromMe"] == true || (!me.is_empty() && from.rsplit('/').next() == Some(me)),
        sent: millis(&id) / 1000,
    })
}

/// A reply chain's title from its post's text: the subject line, else its first words.
fn chain_title(post: &str) -> String {
    let first = post.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    match first.chars().count() > CHAIN_TITLE {
        true => format!("{}…", first.chars().take(CHAIN_TITLE).collect::<String>().trim_end()),
        false => first.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_chain_is_titled_by_its_post_and_a_meeting_by_its_subject() {
        assert_eq!(chain_title("Release train\nWho drives this week?"), "Release train");
        assert_eq!(chain_title("A post with no subject that goes on for quite a while"), "A post with no subject that goes on for…");
        let meeting = json!({"id": "19:meeting_abc@thread.v2", "title": null, "meetingInformation": {"subject": "Weekly sync"}});
        assert!(is_meeting(&meeting));
        assert_eq!(title_of(&meeting), "Weekly sync");
        assert!(!is_meeting(&json!({"id": "19:grp@thread.v2"})));
    }

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
