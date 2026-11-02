//! What depends on the Space on screen: the one server subscription, the visible window of a
//! member list, and searching a server's members by name.
//!
//! Discord sends a large server's messages, typing and member list only to a session that
//! subscribed to it. The web client subscribes to the server being looked at and nothing else;
//! so does this. A server is never enumerated: member rows are asked for by visible range, a
//! hundred at a time, and search is by a name the user typed.
//!
//! The subscription frame (op 37) is not in Discord's own documentation. Its shape here is the
//! community's description of the web client and is unconfirmed against a real account.

use super::super::{Failure, Members, Refusal, Reply};
use super::model::{Member, PresenceUpdate};
use super::{answer, gateway, Discord};
use protocol::{
    ConversationKind, MediaFile, MemberSection, Network, Person, Presence, Relation, Watching, SEARCH_MIN_CHARS,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Rows per range, Discord's own window.
const WINDOW: u32 = 100;
/// Most rows of the open list kept: the three windows the web client holds at once, and slack.
const ROSTER: usize = 400;
/// People outside the user's own lists whose presence one subscription asks for.
const PEOPLE: usize = 100;
const PATIENCE: Duration = Duration::from_secs(10);
/// How long a Space stays subscribed with no word from the shell about what is on screen.
const LINGER: Duration = Duration::from_secs(600);
/// A server's members are searched (op 8) at most once in this long.
const SEARCH_AGAIN: Duration = Duration::from_secs(30);
/// And the account as a whole at most once in this long.
pub(super) const SEARCH_GAP: Duration = Duration::from_millis(400);
/// Searches remembered as answered, so the same words are never sent twice in a session.
const SEARCHED: usize = 512;

/// Searching a server's members by name: what was sent, what waits its turn, what is done.
#[derive(Default)]
pub struct Search {
    /// Sent and not answered yet: the words, the nonce (so no other chunk is ever stored), when.
    sent: Option<(String, String, Instant)>,
    /// Typed while the last search was too recent: the newest only, sent when it may be.
    waiting: Option<String>,
    /// When each server was last asked, and when any was.
    asked: HashMap<String, Instant>,
    last: Option<Instant>,
    /// Server and words already answered in this session.
    done: HashSet<(String, String)>,
}

/// What Discord was last told about the one subscribed server.
#[derive(Clone, PartialEq)]
struct Subscription {
    guild: String,
    channel: Option<String>,
    ranges: Vec<(u32, u32)>,
    people: Vec<String>,
}

enum Entry {
    /// A section header; it takes a row of Discord's list.
    Header,
    Member(Box<Person>),
}

/// The visible part of the open channel's member list, kept current by Discord's own updates
/// and dropped when the channel closes.
struct Roster {
    channel: String,
    /// Sections in order: role id (or `online` / `offline`) and how many people it holds.
    groups: Vec<(String, u32)>,
    rows: BTreeMap<u32, Entry>,
}

#[derive(Default)]
pub struct Watched {
    /// The server whose tree or channel is on screen.
    pub on_screen: Option<String>,
    /// The channel whose member list is showing.
    list: Option<String>,
    ranges: Vec<(u32, u32)>,
    people: Vec<String>,
    sent: Option<Subscription>,
    roster: Option<Roster>,
    /// The shell's screen as last acted on, so a heartbeat that finds it unchanged does nothing.
    seen: Arc<Watching>,
    /// When the shell last asked for the Space's tree.
    opened: Option<Instant>,
}

impl Watched {
    /// A new gateway session knows of no subscription.
    pub fn forget_sent(&mut self) {
        self.sent = None;
        self.roster = None;
    }

    fn show(&mut self, guild: Option<&str>) {
        if self.on_screen.as_deref() != guild {
            *self = Watched {
                on_screen: guild.map(str::to_owned),
                sent: self.sent.take(),
                seen: self.seen.clone(),
                opened: self.opened,
                ..Watched::default()
            };
        }
    }

    fn list(&mut self, channel: Option<&str>) {
        if self.list.as_deref() != channel {
            self.list = channel.map(str::to_owned);
            self.ranges = if channel.is_some() { vec![(0, WINDOW - 1)] } else { Vec::new() };
            self.roster = None;
        }
    }
}

impl Roster {
    /// The row of Discord's list that member number `member` is on: headers take rows too.
    fn row_of(&self, member: u32) -> u32 {
        let mut before = 0;
        for (index, (_, count)) in self.groups.iter().enumerate() {
            if member < before + count {
                return member + index as u32 + 1;
            }
            before += count;
        }
        member + self.groups.len() as u32
    }

    fn total(&self) -> u32 {
        self.groups.iter().map(|(_, count)| count).sum()
    }

    /// The members asked for, if every one of them is here.
    fn window(&self, offset: u32, limit: u32) -> Option<Vec<Person>> {
        let end = offset.saturating_add(limit).min(self.total());
        let rows = (offset..end).map(|member| match self.rows.get(&self.row_of(member)) {
            Some(Entry::Member(person)) => Some((**person).clone()),
            _ => None,
        });
        rows.collect()
    }
}

/// The ranges that hold members `offset..offset + limit`: always the first window, as the web
/// client keeps it, and the one or two the rows fall in.
fn ranges(roster: Option<&Roster>, offset: u32, limit: u32) -> Vec<(u32, u32)> {
    let last = offset + limit.max(1) - 1;
    let (first_row, last_row) = match roster {
        Some(roster) => (roster.row_of(offset), roster.row_of(last.min(roster.total().max(1) - 1))),
        // Before the sections are known: a guess, corrected by the first answer.
        None => (offset, last + 8),
    };
    let mut ranges = vec![(0, WINDOW - 1)];
    for window in (first_row / WINDOW)..=(last_row / WINDOW) {
        let range = (window * WINDOW, window * WINDOW + WINDOW - 1);
        if !ranges.contains(&range) && ranges.len() < 3 {
            ranges.push(range);
        }
    }
    ranges
}

#[derive(Deserialize)]
struct ListGroup {
    id: String,
    #[serde(default)]
    count: u32,
}

#[derive(Deserialize)]
struct ListItem {
    #[serde(default)]
    member: Option<Member>,
}

#[derive(Deserialize)]
struct ListOp {
    op: String,
    #[serde(default)]
    range: Option<(u32, u32)>,
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    item: Option<ListItem>,
}

/// One operation, with its rows already made: what, the range or the row, the entries.
type Applied = (String, Option<(u32, u32)>, Option<u32>, Vec<Entry>);

#[derive(Deserialize)]
pub struct ListUpdate {
    guild_id: String,
    #[serde(default)]
    groups: Vec<ListGroup>,
    #[serde(default)]
    ops: Vec<ListOp>,
}

impl ListUpdate {
    fn changes_roster(&self, groups: &[(String, u32)]) -> bool {
        // SYNC delivers a requested window and INVALIDATE discards an old one.
        // Neither edits the roster; treating them as edits makes readers
        // invalidate the page that just arrived and starves later queued pages.
        !self.groups.iter().map(|group| (&group.id, group.count)).eq(groups.iter().map(|(id, count)| (id, *count)))
            || self.ops.iter().any(|op| matches!(op.op.as_str(), "UPDATE" | "INSERT" | "DELETE"))
    }
}

#[derive(Deserialize)]
pub struct Chunk {
    #[serde(default)]
    members: Vec<Member>,
    #[serde(default)]
    presences: Vec<PresenceUpdate>,
    #[serde(default)]
    nonce: Option<String>,
}

impl Discord {
    /// Tells Discord what is on screen, if that differs from what it was last told: one frame
    /// per navigation.
    pub(super) fn sync_subscription(&self) {
        self.subscribe(false);
    }

    fn subscribe(&self, again: bool) {
        let frame = {
            let mut state = self.state.lock().unwrap();
            let watched = &mut state.members;
            let want = watched.on_screen.clone().map(|guild| Subscription {
                guild,
                channel: watched.list.clone(),
                ranges: watched.ranges.clone(),
                people: watched.people.clone(),
            });
            if want == watched.sent && !again {
                return;
            }
            let mut subscriptions = Map::new();
            if let Some(old) = watched.sent.as_ref().filter(|old| Some(&old.guild) != want.as_ref().map(|want| &want.guild)) {
                let off = json!({
                    "typing": false, "threads": false, "activities": false, "member_updates": false,
                    "members": [], "channels": {}, "thread_member_lists": [],
                });
                subscriptions.insert(old.guild.clone(), off);
            }
            if let Some(want) = &want {
                let mut channels = Map::new();
                if let Some(channel) = &want.channel {
                    let ranges: Vec<Value> = want.ranges.iter().map(|(from, to)| json!([from, to])).collect();
                    channels.insert(channel.clone(), Value::Array(ranges));
                }
                // Activities stay off: they are a stream of what every member is playing.
                let on = json!({
                    "typing": true, "threads": true, "activities": false, "member_updates": false,
                    "members": want.people, "channels": channels, "thread_member_lists": [],
                });
                subscriptions.insert(want.guild.clone(), on);
            }
            watched.sent = want;
            json!({"subscriptions": subscriptions})
        };
        self.tell(37, frame);
    }

    pub(super) fn open_space(&self, guild: &str) {
        {
            let mut state = self.state.lock().unwrap();
            if !state.guilds.contains_key(guild) {
                return;
            }
            state.members.show(Some(guild));
            state.members.opened = Some(Instant::now());
        }
        self.sync_subscription();
    }

    /// The shell's screen changed. The Space it names, or the server of the open conversation,
    /// is the one subscribed; a direct conversation, another network, or a shell that went away
    /// ends the subscription.
    pub(super) fn watch(&self, watching: &Arc<Watching>) {
        {
            let mut state = self.state.lock().unwrap();
            let watched = &mut state.members;
            if *watched.seen == **watching {
                // Nothing moved. A Space opened by a shell that then never said what it is
                // looking at is let go after a while, so a shell that quit leaves nothing held.
                let idle = **watching == Watching::default() && watched.opened.is_some_and(|at| at.elapsed() > LINGER);
                if !idle || watched.on_screen.is_none() {
                    return;
                }
            }
            watched.seen = watching.clone();
        }
        self.place(watching);
    }

    /// The screen the shell last named, placed again now that READY has said what its servers
    /// and channels are: a Watch that came before READY found none of them, and the same Watch
    /// again is not acted on.
    pub(super) fn rewatch(&self) {
        let seen = self.state.lock().unwrap().members.seen.clone();
        if *seen == Watching::default() {
            // Nothing named; a Space opened without a Watch stays as it is.
            return self.sync_subscription();
        }
        self.place(&seen);
    }

    /// Works out what `watching` puts on screen and subscribes it.
    fn place(&self, watching: &Arc<Watching>) {
        let ours = |key: &protocol::Key| key.network == Network::Discord && key.account == self.ctx.account.account;
        let open = watching.conversation.as_ref().filter(|key| ours(key));
        let conversation = open.and_then(|key| self.ctx.known(&key.id).ok().flatten());
        let guild = conversation.as_ref().and_then(|conversation| self.guild_of(conversation));
        let listable = conversation.as_ref().is_some_and(|conversation| conversation.kind != ConversationKind::Thread);
        {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            // A newer Watch came meanwhile on the other thread; it places itself.
            if !Arc::ptr_eq(&state.members.seen, watching) {
                return;
            }
            // The tree on screen is named outright; an open conversation says it too.
            let named = watching.space.as_ref().map(|space| ours(space).then(|| space.id.clone()));
            let guild = guild.filter(|guild| state.guilds.contains_key(guild));
            match (named, &watching.conversation, &guild) {
                (Some(space), _, _) => {
                    let space = space.filter(|space| state.guilds.contains_key(space));
                    state.members.show(space.as_deref());
                    let list = open.filter(|_| watching.members.is_some() && listable && guild == space);
                    state.members.list(list.map(|key| key.id.as_str()));
                }
                (None, Some(_), Some(guild)) => {
                    state.members.show(Some(guild));
                    let list = open.filter(|_| watching.members.is_some() && listable);
                    state.members.list(list.map(|key| key.id.as_str()));
                }
                (None, Some(_), None) => state.members.show(None),
                (None, None, _) if **watching == Watching::default() => state.members.show(None),
                (None, None, _) => state.members.list(None),
            }
            if state.members.list.is_some() {
                if let Some((offset, count)) = watching.members {
                    // Round to GUI cache pages so both edge pages are available.
                    let first = offset / 50 * 50;
                    let end = offset.saturating_add(count).saturating_add(49) / 50 * 50;
                    state.members.ranges = ranges(state.members.roster.as_ref(), first, end.saturating_sub(first));
                }
            }
            let listed: HashSet<String> = state.members.roster.as_ref().into_iter().flat_map(|roster| roster.rows.iter())
                .filter(|(row, _)| state.members.ranges.iter().any(|(from, to)| *row >= from && *row <= to))
                .filter_map(|(_, entry)| match entry { Entry::Member(person) => Some(person.key.id.clone()), _ => None }).collect();
            // Friends' presence arrives unasked. Anyone else on screen is asked for by id, and
            // only as part of the server that is on screen anyway.
            let relations = &state.relations;
            let strangers = watching.people.iter().filter(|key| ours(key)).map(|key| &key.id);
            let strangers = strangers.filter(|id| !listed.contains(*id) && relations.get(*id) != Some(&Relation::Friend) && **id != state.me);
            state.members.people = match state.members.on_screen {
                Some(_) => strangers.take(PEOPLE).cloned().collect(),
                None => Vec::new(),
            };
        }
        self.sync_subscription();
    }

    fn sections(&self, guild: &str, roster: &Roster) -> Vec<MemberSection> {
        let state = self.state.lock().unwrap();
        let roles = state.guilds.get(guild).map(|info| &info.roles);
        let named = roster.groups.iter().map(|(id, count)| MemberSection {
            name: match id.as_str() {
                "online" => "Online".to_owned(),
                "offline" => "Offline".to_owned(),
                role => {
                    let role = roles.and_then(|roles| roles.get(&super::model::number(role)));
                    role.map_or_else(|| "Members".to_owned(), |role| role.name.clone())
                }
            },
            count: *count,
        });
        named.collect()
    }

    /// Reads the current cache immediately. Only Watch controls the live viewport;
    /// a cache miss is pending data, never a request that blocks later scrolling.
    pub(super) fn members(&self, channel: String, offset: u32, limit: u32, reply: Reply<Members>) {
        let conversation = self.ctx.known(&channel).ok().flatten();
        let guild = conversation.as_ref().and_then(|conversation| self.guild_of(conversation));
        let in_tree = conversation.as_ref().is_some_and(|conversation| conversation.space.is_some());
        let (Some(guild), true) = (guild, in_tree) else { return answer(reply, Err(Refusal::Unsupported)); };
        if conversation.is_some_and(|conversation| conversation.kind == ConversationKind::Thread) {
            return answer(reply, Err(Refusal::Unsupported));
        }
        let (groups, members, pending) = {
            let mut state = self.state.lock().unwrap();
            if !state.guilds.contains_key(&guild) {
                if !state.guilds.is_empty() { return answer(reply, Err(Refusal::Unsupported)); }
                // Before READY no server is known yet: the list is still to come, not refused.
                // Nothing is placed here; READY places the Watch once it has said what the servers are.
                return answer(reply, Ok(Members { pending: true, sections: Vec::new(), members: Vec::new() }));
            }
            let watched = &mut state.members;
            if watched.seen.members.is_some() && watched.list.as_deref() != Some(&channel) {
                // A delayed query from a previous conversation cannot take over
                // a viewport already selected by Watch.
                return answer(reply, Ok(Members { pending: true, sections: Vec::new(), members: Vec::new() }));
            }
            let opening = watched.list.as_deref() != Some(&channel);
            watched.show(Some(&guild));
            watched.list(Some(&channel));
            // Bootstrap the first window before its sections make a viewport possible.
            // Subsequent cache queries must never move the subscribed window.
            if opening { watched.ranges = ranges(watched.roster.as_ref(), offset, limit.clamp(1, WINDOW)); }
            let groups = watched.roster.as_ref().map_or_else(Vec::new, |roster| roster.groups.clone());
            let found = watched.roster.as_ref().and_then(|roster| roster.window(offset, limit.clamp(1, WINDOW)));
            let pending = found.is_none();
            (groups, found.unwrap_or_default(), pending)
        };
        self.sync_subscription();
        let roster = Roster { channel, groups, rows: BTreeMap::new() };
        answer(reply, Ok(Members { pending, sections: self.sections(&guild, &roster), members }));
    }

    fn person(&self, member: &Member, files: &mut Vec<(MediaFile, String)>) -> Option<(Person, Presence, String)> {
        let user = member.user.as_ref()?;
        let (avatar, url) = user.avatar();
        files.push((avatar.clone(), url));
        let presence = member.presence.as_ref();
        let (shown, status) = presence.map_or((Presence::Unknown, ""), |presence| (presence.presence(), presence.status()));
        let person = Person {
            link: super::model::user_link(&user.id),
            key: self.ctx.key(&user.id),
            name: member.name().to_owned(),
            handle: user.username.clone(),
            avatar: Some(avatar),
            bot: user.bot,
            relation: Relation::None,
            presence: shown,
            status: status.to_owned(),
            ..Person::new(self.ctx.key(&user.id), "")
        };
        Some((person, shown, status.to_owned()))
    }

    /// Discord's updates to the list we subscribed to: a window synced, a row inserted, changed
    /// or removed.
    pub(super) fn member_list(&self, update: ListUpdate) {
        let mut files = Vec::new();
        let mut seen = Vec::new();
        let shown;
        let changed;
        let mut entry = |item: &ListItem| match item.member.as_ref().and_then(|member| self.person(member, &mut files)) {
            Some((person, presence, status)) => {
                seen.push((person.key.id.clone(), presence, status));
                Entry::Member(Box::new(person))
            }
            None => Entry::Header,
        };
        // Converted before the lock: `person` reads nothing from the state.
        let ops: Vec<Applied> = update
            .ops
            .iter()
            .map(|op| {
                let entries = op.items.iter().chain(&op.item).map(&mut entry).collect();
                (op.op.clone(), op.range, op.index, entries)
            })
            .collect();
        {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let watched = &mut state.members;
            let (Some(guild), Some(channel)) = (&watched.on_screen, &watched.list) else {
                return;
            };
            if *guild != update.guild_id {
                return;
            }
            let roster = watched.roster.get_or_insert_with(|| Roster {
                channel: channel.clone(),
                groups: Vec::new(),
                rows: BTreeMap::new(),
            });
            if roster.channel != *channel {
                return;
            }
            shown = channel.clone();
            changed = update.changes_roster(&roster.groups);
            roster.groups = update.groups.iter().map(|group| (group.id.clone(), group.count)).collect();
            for (op, range, index, mut entries) in ops {
                match (op.as_str(), range, index) {
                    ("SYNC", Some((from, _)), _) => {
                        for (offset, entry) in entries.into_iter().enumerate() {
                            roster.rows.insert(from + offset as u32, entry);
                        }
                    }
                    ("INVALIDATE", Some((from, to)), _) => roster.rows.retain(|row, _| *row < from || *row > to),
                    ("UPDATE", _, Some(index)) => {
                        if let Some(entry) = entries.pop() {
                            roster.rows.insert(index, entry);
                        }
                    }
                    ("INSERT" | "DELETE", _, Some(index)) => {
                        // Every row from here on moves by one.
                        let inserted = op == "INSERT";
                        let tail = roster.rows.split_off(&index);
                        let moved = tail.into_iter().filter(|(row, _)| inserted || *row != index);
                        let moved = moved.map(|(row, entry)| (if inserted { row + 1 } else { row - 1 }, entry));
                        roster.rows.extend(moved);
                        if let Some(entry) = entries.pop().filter(|_| inserted) {
                            roster.rows.insert(index, entry);
                        }
                    }
                    _ => {}
                }
            }
            // Only the windows asked for are kept.
            if roster.rows.len() > ROSTER {
                let ranges = &watched.ranges;
                roster.rows.retain(|row, _| ranges.iter().any(|(from, to)| row >= from && row <= to));
            }
            for entry in roster.rows.values_mut() {
                if let Entry::Member(person) = entry {
                    person.relation = state.relations.get(&person.key.id).copied().unwrap_or_default();
                }
            }
        }
        if let Err(err) = self.ctx.media(&files) {
            self.log("member list", err);
        }
        // chatd keeps these only for the people the shell is watching.
        for (id, presence, status) in seen {
            self.ctx.presence(&id, presence, &status, false);
        }
        // Wake cache readers when their subscribed data arrives, as well as on edits.
        if changed || update.ops.iter().any(|op| op.op == "SYNC") {
            self.ctx.members_changed(&shown);
        }
    }

    /// Members of the server on screen whose name starts with what the user typed. The result
    /// arrives as a chunk and goes to People; at most 25. Discord has no directory outside a
    /// server, so with no server on screen there is nothing to ask.
    ///
    /// Paced on every side: at least two characters, the same words never twice in a session,
    /// one search per account in [`SEARCH_GAP`] and one per server in [`SEARCH_AGAIN`]. What is
    /// typed too soon waits (the newest only) and is sent by `search_tick` when it may be.
    pub(super) fn search_people(&self, query: &str) {
        let query = query.trim();
        if query.chars().count() < SEARCH_MIN_CHARS {
            return self.ctx.searched(query);
        }
        let mut state = self.state.lock().unwrap();
        let state = &mut *state;
        let Some(guild) = state.members.on_screen.clone() else {
            drop(state.search.waiting.take());
            return self.ctx.searched(query);
        };
        let search = &mut state.search;
        if search.done.contains(&(guild.clone(), query.to_lowercase())) {
            return self.ctx.searched(query);
        }
        if search.sent.as_ref().is_some_and(|(sent, _, _)| sent == query) {
            return;
        }
        let soon = |at: Option<&Instant>, gap: Duration| at.is_some_and(|at| at.elapsed() < gap);
        if search.sent.is_some() || soon(search.last.as_ref(), SEARCH_GAP) || soon(search.asked.get(&guild), SEARCH_AGAIN) {
            // Whatever waited before is overtaken: it is done, with what the cache has.
            if let Some(overtaken) = search.waiting.replace(query.to_owned()).filter(|before| before != query) {
                self.ctx.searched(&overtaken);
            }
            return;
        }
        let nonce = (gateway::random() % 1_000_000_000_000).to_string();
        search.sent = Some((query.to_owned(), nonce.clone(), Instant::now()));
        search.last = Some(Instant::now());
        search.asked.insert(guild.clone(), Instant::now());
        self.tell(8, json!({"guild_id": [guild], "query": query, "limit": 25, "presences": true, "nonce": nonce}));
    }

    /// Sends the search that waited once it may go, and gives up on one Discord never
    /// answered. True while either is outstanding, so the socket thread looks again soon.
    pub(super) fn search_tick(&self) -> bool {
        let (unanswered, due) = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let late = state.search.sent.as_ref().is_some_and(|(_, _, at)| at.elapsed() > PATIENCE);
            let unanswered = state.search.sent.take_if(|_| late).map(|(query, _, _)| query);
            let guild = state.members.on_screen.as_ref();
            let free = state.search.sent.is_none()
                && state.search.last.is_none_or(|at| at.elapsed() >= SEARCH_GAP)
                && guild.is_none_or(|guild| state.search.asked.get(guild).is_none_or(|at| at.elapsed() >= SEARCH_AGAIN));
            (unanswered, state.search.waiting.take_if(|_| free))
        };
        if let Some(query) = unanswered {
            self.ctx.searched(&query);
        }
        if let Some(query) = due {
            self.search_people(&query);
        }
        let state = self.state.lock().unwrap();
        state.search.sent.is_some() || state.search.waiting.is_some()
    }

    pub(super) fn members_chunk(&self, chunk: Chunk) -> Result<(), Failure> {
        let asked = {
            let state = self.state.lock().unwrap();
            state.search.sent.as_ref().map(|(query, nonce, _)| (query.clone(), nonce.clone()))
        };
        let Some((query, _)) = asked.filter(|(_, nonce)| chunk.nonce.as_ref() == Some(nonce)) else {
            return Ok(());
        };
        let mut files = Vec::new();
        let mut people = Vec::new();
        for user in chunk.members.iter().filter_map(|member| member.user.as_ref()).take(25) {
            // People keeps who someone is everywhere: a nickname we gave a friend, else their
            // own name. A server nickname belongs to that server's messages and member list.
            let nickname = self.state.lock().unwrap().users.get(&user.id).cloned();
            files.push(user.avatar());
            people.push(user.person(nickname.as_deref()));
        }
        self.ctx.media(&files)?;
        self.ctx.people(&people)?;
        for presence in &chunk.presences {
            self.presence(presence);
        }
        {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            state.search.sent = None;
            if state.search.done.len() >= SEARCHED {
                state.search.done.clear();
            }
            if let Some(guild) = &state.members.on_screen {
                state.search.done.insert((guild.clone(), query.to_lowercase()));
            }
        }
        self.ctx.searched(&query);
        Ok(())
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;

    #[test]
    fn requested_windows_do_not_invalidate_the_pages_they_deliver() {
        let mut update: ListUpdate = serde_json::from_value(json!({
            "guild_id": "guild", "groups": [{"id": "online", "count": 200}],
            "ops": [{"op": "SYNC", "range": [100, 199], "items": []}]
        })).unwrap();
        let groups = vec![("online".to_owned(), 200)];
        assert!(!update.changes_roster(&groups));
        // Every page in a multi-page viewport must be able to land without
        // forcing the reader back to the first page on each delivery.
        for range in [(0, 99), (100, 199), (200, 299)] {
            update.ops[0].range = Some(range);
            assert!(!update.changes_roster(&groups));
        }
        update.ops[0].op = "INVALIDATE".to_owned();
        assert!(!update.changes_roster(&groups), "discarding an old window must not restart visible reads");
        update.groups[0].count = 201;
        assert!(update.changes_roster(&groups), "section counts still invalidate cached offsets");
        update.groups[0].count = 200;
        for op in ["UPDATE", "INSERT", "DELETE"] {
            update.ops[0].op = op.to_owned();
            assert!(update.changes_roster(&groups), "{op} must refresh cached members");
        }
    }
}
