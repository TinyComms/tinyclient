//! People on a workspace: who wrote a message, who a direct message is with, who is typing or
//! online. The directory is searched and looked up by id, never listed.

use super::emoji;
use super::{https_host, within, Fault, Slack, SEARCHED, SEARCH_EVERY, UNKNOWN};
use crate::adapters::{Failure, Members};
use crate::store::{ConversationRow, PersonDetails, PersonRow};
use protocol::{Activity, ActivityKind, Mark, MediaFile, MediaKind, MemberSection, Person, Presence, Relation, SEARCH_MIN_CHARS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// People remembered in memory between store lookups.
const REMEMBERED: usize = 1_000;
/// Ids per lookup of the edge cache, the web client's own batched `users.info`.
const BATCH: usize = 50;
/// One-by-one lookups made for a single page when the batch is refused.
const SINGLES: usize = 20;
/// Avatars are registered at list size, `image_72`, with `image_192` beside it for a screen
/// that draws them larger (the 80 pt profile card at 2x).
const AVATAR: u32 = 72;
const AVATAR_LARGE: u32 = 192;
/// Members asked for per call, and how far down a member list is ever walked.
const MEMBERS_PAGE: &str = "200";
const ROSTER: usize = 1_000;
/// The form of the note a person is kept in. A note written in an older form is not trusted
/// and the person is looked up once more: it may name a picture that was never registered, or
/// none for someone Slack does serve one for.
const NOTE: u8 = 4;

/// What tinyclient keeps of a person in memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct User {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "h", default)]
    pub handle: String,
    /// The id of the registered avatar, empty when Slack serves none.
    #[serde(rename = "a", default)]
    pub avatar: String,
    #[serde(rename = "b", default)]
    pub bot: bool,
    #[serde(rename = "s", default)]
    pub status: String,
    /// Guest, from another organization, deactivated. Absent from notes written before it.
    #[serde(rename = "m", default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
    /// The status emoji alone, and whether they are in a huddle, told again on each connect.
    #[serde(rename = "e", default, skip_serializing_if = "String::is_empty")]
    pub emoji: String,
    #[serde(rename = "u", default, skip_serializing_if = "std::ops::Not::not")]
    pub huddle: bool,
}

/// Slackbot is an app, though Slack does not flag it as one.
const SLACKBOT: &str = "USLACKBOT";

/// The one tag after a person's name, from fields of the user object already fetched.
/// Unverified against a real workspace: `is_stranger` and the profile's `team` for someone of
/// another organization are the community's reading of Slack Connect users.
fn mark_of(user: &Value, workspace: &str) -> Option<Mark> {
    let team = [&user["team_id"], &user["profile"]["team"]].into_iter().filter_map(Value::as_str).find(|team| !team.is_empty());
    // On Enterprise Grid a colleague's team is another workspace of the same organization.
    let org = user["enterprise_user"].is_object();
    if user["deleted"] == true {
        Some(Mark::Deactivated)
    } else if user["is_stranger"] == true || (team.is_some_and(|team| team != workspace) && !org) {
        Some(Mark::External)
    } else if user["is_restricted"] == true || user["is_ultra_restricted"] == true {
        Some(Mark::Guest)
    } else {
        None
    }
}

/// In a huddle now, as the profile says. Unverified: `huddle_state` and its value are the
/// community's; Slack documents neither.
fn huddling(profile: &Value) -> bool {
    profile["huddle_state"] == "in_a_huddle"
}

fn huddle(on: bool) -> Option<Activity> {
    on.then(|| Activity {
        kind: ActivityKind::Huddle,
        name: String::new(),
        place: String::new(),
        conversation: None,
    })
}

/// A person as kept in a note, so a restart asks Slack nothing: what is remembered in memory,
/// and where the picture is fetched from, so its file can be registered again without asking.
#[derive(Serialize, Deserialize)]
struct Kept {
    #[serde(flatten)]
    user: User,
    #[serde(rename = "v", default)]
    form: u8,
    #[serde(rename = "p", default, skip_serializing_if = "String::is_empty")]
    picture: String,
    #[serde(rename = "q", default, skip_serializing_if = "String::is_empty")]
    large: String,
}

/// The member list being read: the ids walked so far and where Slack's paging stands, so the
/// next window continues from there and never starts over.
pub(super) struct Roster {
    channel: String,
    ids: Vec<String>,
    /// Where the next page starts. `None` once the list has ended or the walk has gone as far
    /// as it ever will.
    cursor: Option<String>,
    /// How many members Slack says there are, asked once.
    count: Option<u32>,
}

/// `%2F` and the like as the characters they stand for.
fn unescaped(text: &str) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut at) = (Vec::with_capacity(bytes.len()), 0);
    while at < bytes.len() {
        let hex = bytes.get(at + 1..at + 3).and_then(|hex| std::str::from_utf8(hex).ok());
        match (bytes[at], hex.and_then(|hex| u8::from_str_radix(hex, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The picture Slack itself shows for someone who uploaded none. Their profile then names
/// gravatar, with Slack's own default picture as the `d=` fallback:
/// `https://secure.gravatar.com/avatar/<hash>.jpg?s=72&d=https%3A%2F%2Fa.slack-edge.com%2F..%2Fava_0010-72.png`.
/// Gravatar is not one of Slack's hosts and answers with a redirect, which the media cache
/// refuses on purpose, so the default is what is registered. Someone who does have a gravatar
/// shows Slack's default here rather than it.
fn gravatar_default(url: &str) -> Option<String> {
    if !https_host(url).is_some_and(|host| within(host, "gravatar.com")) {
        return None;
    }
    let query = url.split_once('?')?.1;
    let fallback = query.split('&').find_map(|pair| pair.strip_prefix("d="))?;
    Some(unescaped(fallback))
}

impl User {
    pub fn avatar(&self) -> Option<MediaFile> {
        (!self.avatar.is_empty()).then(|| MediaFile {
            width: AVATAR,
            height: AVATAR,
            ..MediaFile::new(&self.avatar, MediaKind::Avatar)
        })
    }
}

impl Slack {
    /// Slack's own link to a person, which the system hands to the Slack app.
    fn person_link(&self, id: &str) -> String {
        format!("slack://user?team={}&id={id}", self.session.team_id)
    }

    fn person_row(&self, id: &str, user: &User) -> PersonRow {
        PersonRow {
            link: self.person_link(id),
            id: id.to_owned(),
            name: user.name.clone(),
            handle: user.handle.clone(),
            avatar: Some(user.avatar.clone()).filter(|avatar| !avatar.is_empty()),
            bot: user.bot,
            status: user.status.clone(),
        }
    }

    /// The web client's own cache service, which answers lookups and searches in batches.
    /// Unverified against a real workspace: the shapes follow slackdump's edge client.
    fn edge(&self, path: &str, mut body: Value) -> Result<Value, Fault> {
        let method = format!("edge:{path}");
        self.halted(&method)?;
        body["token"] = self.session.token.as_str().into();
        let url = format!("{}{}/{path}", self.edge, self.ctx.account.account);
        if self.pace(&method) {
            return Err(Fault::Net(format!("{path}: stopped")));
        }
        let sent = self.agent.post(&url).header("Cookie", self.cookie()).send_json(&body);
        let mut response = sent.map_err(|err| Fault::Net(err.to_string()))?;
        self.reissued(response.headers());
        if response.status().as_u16() == 429 {
            // Not retried: every caller has a documented call to fall back on, or can wait.
            let wait = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
            return Err(Fault::Limited(wait.unwrap_or(5)));
        }
        let body: Value = response.body_mut().read_json().map_err(|err| Fault::Net(err.to_string()))?;
        self.checked(path, body)
    }

    /// The address of a person's picture at one of Slack's sizes: their own when Slack serves
    /// it, else Slack's default for them.
    fn picture(&self, id: &str, profile: &Value, size: u32) -> Option<String> {
        let Some(url) = profile[format!("image_{size}")].as_str() else {
            // What the web client's own lookup returns carries no addresses, only the hash the
            // web client builds them from. Seen on a real workspace: every profile was like this.
            let hash = profile["avatar_hash"].as_str().filter(|hash| !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_alphanumeric()))?;
            let team = profile["team"].as_str().filter(|team| !team.is_empty() && team.bytes().all(|b| b.is_ascii_alphanumeric()))?;
            return Some(format!("https://ca.slack-edge.com/{team}-{id}-{hash}-{size}"));
        };
        if self.hosted(url) {
            return Some(url.to_owned());
        }
        gravatar_default(url).filter(|fallback| self.hosted(fallback))
    }

    /// Registers a person's picture: once at list size, and the larger one as its variant.
    fn register_avatar(&self, id: &str, picture: &str, large: &str) -> rusqlite::Result<()> {
        let file = MediaFile {
            width: AVATAR,
            height: AVATAR,
            ..MediaFile::new(id, MediaKind::Avatar)
        };
        self.ctx.media(&[(file, picture.to_owned())])?;
        if !large.is_empty() && large != picture {
            self.ctx.media_variants(id, &[(AVATAR_LARGE, large.to_owned())])?;
        }
        Ok(())
    }

    fn remember(&self, id: &str, user: &User) {
        let mut state = self.state();
        if state.users.len() >= REMEMBERED {
            state.users.clear();
        }
        state.users.insert(id.to_owned(), user.clone());
    }

    /// Reads Slack's user objects, registers their avatars and records them as people. The
    /// file is registered before the person who names it, and the person before any message
    /// or row of theirs.
    pub(super) fn learn(&self, users: &[Value]) -> Vec<(String, User)> {
        let (mut learned, mut rows, mut details, mut doing) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for user in users {
            let Some(id) = user["id"].as_str() else {
                continue;
            };
            let profile = &user["profile"];
            if std::env::var_os("TINYCLIENT_SHAPES").is_some() {
                // Field names only, never values: for comparing what Slack sends with what is read.
                let keys = |value: &Value| value.as_object().map(|map| map.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default();
                eprintln!("chatd: Slack: user shape [{}] profile [{}]", keys(user), keys(profile));
            }
            // Slack's rule for the name it shows: the display name, else the real one.
            let names = [&profile["display_name"], &profile["real_name"], &user["real_name"], &user["name"]];
            let name = names.into_iter().filter_map(Value::as_str).find(|name| !name.is_empty());
            let large = self.picture(id, profile, AVATAR_LARGE).unwrap_or_default();
            let picture = self.picture(id, profile, AVATAR).or_else(|| self.picture(id, profile, 48)).unwrap_or_else(|| large.clone());
            let avatar = (!picture.is_empty()).then(|| {
                // A changed picture is a new hash, so a new id.
                let hash = profile["avatar_hash"].as_str().map_or_else(|| format!("{:x}", super::fnv(&picture)), str::to_owned);
                format!("avatar:{id}:{hash}")
            });
            if let Some(avatar) = &avatar {
                if let Err(err) = self.register_avatar(avatar, &picture, &large) {
                    eprintln!("chatd: Slack: {err}");
                }
            }
            let status_emoji = profile["status_emoji"].as_str().unwrap_or("").trim_matches(':');
            let status_emoji = emoji::standard(status_emoji).unwrap_or_default();
            let status = format!("{status_emoji} {}", profile["status_text"].as_str().unwrap_or(""));
            let mark = mark_of(user, &self.session.team_id);
            let known = User {
                name: name.unwrap_or(id).to_owned(),
                handle: user["name"].as_str().unwrap_or("").to_owned(),
                avatar: avatar.unwrap_or_default(),
                bot: user["is_bot"] == true || user["is_app_user"] == true || id == SLACKBOT,
                status: status.trim().to_owned(),
                mark,
                emoji: status_emoji.to_owned(),
                huddle: huddling(profile),
            };
            rows.push(self.person_row(id, &known));
            let title = profile["title"].as_str().unwrap_or("").to_owned();
            let utc_offset = user["tz_offset"].as_i64().map(|offset| offset as i32);
            details.push((id.to_owned(), PersonDetails { title, utc_offset, mark }));
            doing.push((id.to_owned(), status_emoji.to_owned(), huddle(huddling(profile))));
            let kept = Kept {
                user: known.clone(),
                form: NOTE,
                picture,
                large,
            };
            self.ctx.set_note(&format!("u:{id}"), &serde_json::to_string(&kept).unwrap_or_default());
            self.remember(id, &known);
            learned.push((id.to_owned(), known));
        }
        if let Err(err) = self.ctx.people(&rows).and_then(|()| self.ctx.details(&details)) {
            eprintln!("chatd: Slack: {err}");
        }
        // The status emoji and a huddle, kept like presence: only for people the user owns or
        // has on screen.
        for (id, emoji, activity) in doing {
            self.ctx.activity(&id, &emoji, activity, self.owned(&id));
        }
        let mut state = self.state();
        for (id, _) in &learned {
            state.unknown.remove(id);
        }
        drop(state);
        learned
    }

    /// Someone already looked up, from memory or the store. Never asks Slack. The first time a
    /// run reads someone from their note, their picture is registered again from the address
    /// kept there: a person must never name a file the cache does not know.
    pub(super) fn cached(&self, id: &str) -> Option<User> {
        if let Some(user) = self.state().users.get(id) {
            return Some(user.clone());
        }
        let kept: Kept = serde_json::from_str(&self.ctx.note(&format!("u:{id}"))?).ok()?;
        if kept.form != NOTE {
            return None;
        }
        if !kept.user.avatar.is_empty() {
            if let Err(err) = self.register_avatar(&kept.user.avatar, &kept.picture, &kept.large) {
                eprintln!("chatd: Slack: {err}");
            }
        }
        self.remember(id, &kept.user);
        Some(kept.user)
    }

    /// Remembers, for the session, something Slack would not describe.
    pub(super) fn unknown(&self, id: &str) {
        let mut state = self.state();
        if state.unknown.len() >= UNKNOWN {
            state.unknown.clear();
        }
        state.unknown.insert(id.to_owned());
    }

    /// One person, asked of Slack the first time they are seen. Someone Slack will not name
    /// shows as their id rather than holding a message back, and is not asked about again this
    /// session: one lookup, not one per message of theirs.
    pub(super) fn user(&self, id: &str) -> User {
        if let Some(user) = self.cached(id) {
            return user;
        }
        let nameless = || User {
            name: id.to_owned(),
            ..User::default()
        };
        if self.state().unknown.contains(id) || !self.info_due(id) {
            return nameless();
        }
        match self.api("users.info", &[("user", id)]) {
            Ok(info) => {
                // The same answer is the card: opened within the hour, nothing more is asked.
                self.keep_about(id, &info["user"]);
                let mut learned = self.learn(std::slice::from_ref(&info["user"]));
                learned.pop().map_or_else(nameless, |(_, user)| user)
            }
            Err(Fault::Slack(_)) => {
                self.unknown(id);
                nameless()
            }
            // Slack was not reached: nothing was learned about them either way.
            Err(_) => nameless(),
        }
    }

    /// Looks up everyone in `ids` not yet known, in batches, before a page that names them all.
    pub(super) fn users(&self, ids: &[String]) {
        let mut unknown: Vec<&String> = ids.iter().filter(|id| self.cached(id).is_none()).collect();
        unknown.retain(|id| !self.state().unknown.contains(*id));
        unknown.sort();
        unknown.dedup();
        let mut singles = SINGLES;
        for batch in unknown.chunks(BATCH) {
            let wanted: serde_json::Map<String, Value> = batch.iter().map(|id| ((*id).clone(), json!(0))).collect();
            let asked = json!({"check_interaction": true, "include_profile_only_users": true, "updated_ids": wanted});
            let found = match self.edge("users/info", asked) {
                Ok(found) => self.learn(found["results"].as_array().map_or(&[][..], Vec::as_slice)),
                Err(_) => Vec::new(),
            };
            // What the batch did not answer is asked the documented way, a few at a time.
            for id in batch.iter().filter(|id| !found.iter().any(|(known, _)| known == **id)) {
                if singles == 0 {
                    return;
                }
                singles -= 1;
                self.user(id);
            }
        }
    }

    fn person(&self, id: &str, user: &User) -> Person {
        let partner = self.state().partners.contains_key(id);
        Person {
            link: self.person_link(id),
            key: self.ctx.key(id),
            name: user.name.clone(),
            handle: user.handle.clone(),
            avatar: user.avatar(),
            bot: user.bot,
            relation: if partner { Relation::Contact } else { Relation::None },
            presence: self.ctx.presence_of(id),
            status: user.status.clone(),
            ..Person::new(self.ctx.key(id), "")
        }
    }

    /// The workspace directory, searched on Slack's side. At most 25 land in the store. A
    /// query too short to mean anything, or one already searched this session (its results
    /// are in the store), asks Slack nothing; two searches are never closer than
    /// [`SEARCH_EVERY`]. The shell is told the search is over either way.
    pub(super) fn search(&self, query: &str) {
        let asked = query.trim().to_lowercase();
        let wait = {
            let state = self.state();
            let worth = asked.chars().count() >= SEARCH_MIN_CHARS && !state.searched.contains(&asked);
            worth.then(|| state.search_at.map_or(Duration::ZERO, |at| SEARCH_EVERY.saturating_sub(at.elapsed())))
        };
        if let Some(wait) = wait {
            if !wait.is_zero() && self.ctx.pause(wait) {
                return;
            }
            self.state().search_at = Some(Instant::now());
            let wanted = json!({
                "query": asked, "count": 25, "fuzz": 1, "uax29_tokenizer": false, "filter": "NOT deactivated",
            });
            match self.edge("users/search", wanted) {
                Ok(found) => {
                    let results = found["results"].as_array().map_or(&[][..], Vec::as_slice);
                    self.learn(&results[..results.len().min(25)]);
                    let mut state = self.state();
                    if state.searched.len() >= SEARCHED {
                        state.searched.clear();
                    }
                    state.searched.insert(asked);
                }
                Err(fault) => self.report(Err(fault.into())),
            }
        }
        self.ctx.searched(query);
    }

    /// One window of a conversation's members. Slack pages by cursor, so the ids walked so far
    /// are kept with the cursor and a later window continues from there: nothing is fetched
    /// twice, and nothing past the first [`ROSTER`]. Only the window's people are looked up.
    pub(super) fn members(&self, conversation: &str, offset: u32, limit: u32) -> Result<Members, Fault> {
        let channel = super::thread_of(conversation).map_or(conversation, |(channel, _)| channel);
        let held = self.state().roster.take().filter(|roster| roster.channel == channel);
        let mut roster = held.unwrap_or_else(|| Roster {
            channel: channel.to_owned(),
            ids: Vec::new(),
            cursor: Some(String::new()),
            count: None,
        });
        let walked = self.walk(&mut roster, (offset as usize + limit as usize).min(ROSTER));
        let window: Vec<String> = roster.ids.iter().skip(offset as usize).take(limit as usize).cloned().collect();
        let (walked_to, count) = (roster.ids.len(), roster.count);
        // What was walked is kept even when a page failed: the next window goes on from it.
        self.state().roster = Some(roster);
        walked?;
        self.users(&window);
        let members: Vec<Person> = window.iter().map(|id| self.person(id, &self.user(id))).collect();
        Ok(Members {
        pending: false,
            // Slack's member list has no sections.
            sections: vec![MemberSection {
                name: "Members".to_owned(),
                count: count.unwrap_or(walked_to as u32),
            }],
            members,
        })
    }

    /// Reads forward until `wanted` ids are held or the list ends, and asks once how many
    /// members there are in all.
    fn walk(&self, roster: &mut Roster, wanted: usize) -> Result<(), Fault> {
        while roster.ids.len() < wanted {
            let Some(cursor) = roster.cursor.take() else {
                break;
            };
            let asked = [("channel", roster.channel.as_str()), ("limit", MEMBERS_PAGE), ("cursor", cursor.as_str())];
            let listed = match self.api("conversations.members", &asked) {
                Ok(listed) => listed,
                Err(fault) => {
                    roster.cursor = Some(cursor);
                    return Err(fault);
                }
            };
            let page = listed["members"].as_array().into_iter().flatten().filter_map(Value::as_str);
            roster.ids.extend(page.map(str::to_owned));
            roster.ids.truncate(ROSTER);
            let next = listed["response_metadata"]["next_cursor"].as_str().unwrap_or("");
            roster.cursor = (!next.is_empty() && roster.ids.len() < ROSTER).then(|| next.to_owned());
        }
        if roster.count.is_none() {
            let info = self.api("conversations.info", &[("channel", &roster.channel), ("include_num_members", "true")]);
            let counted = info.ok().and_then(|info| info["channel"]["num_members"].as_u64());
            roster.count = Some(counted.map_or(roster.ids.len() as u32, |count| count as u32));
        }
        Ok(())
    }

    /// `member_joined_channel`, `member_left_channel`: the list on screen has a row more or
    /// less. Someone who joins a list still being walked turns up when the walk reaches them.
    pub(super) fn member_moved(&self, event: &Value) {
        let (Some(channel), Some(user)) = (event["channel"].as_str(), event["user"].as_str()) else {
            return;
        };
        let joined = event["type"] == "member_joined_channel";
        {
            let mut state = self.state();
            let Some(roster) = state.roster.as_mut().filter(|roster| roster.channel == channel) else {
                return;
            };
            let at = roster.ids.iter().position(|id| id == user);
            match (joined, at) {
                (true, None) if roster.cursor.is_none() && roster.ids.len() < ROSTER => roster.ids.push(user.to_owned()),
                (false, Some(at)) => drop(roster.ids.remove(at)),
                _ => {}
            }
            if let Some(count) = roster.count.as_mut() {
                *count = if joined { *count + 1 } else { count.saturating_sub(1) };
            }
        }
        self.ctx.members_changed(channel);
    }

    /// The direct conversation with a person, opened on Slack if there was none. Opening sends
    /// nothing to them.
    pub(super) fn start_direct(&self, person: &str) -> Result<String, Fault> {
        let opened = self.api("conversations.open", &[("users", person), ("return_im", "true")])?;
        let Some(id) = opened["channel"]["id"].as_str() else {
            return Err(Fault::Net("conversations.open: no channel".to_owned()));
        };
        self.direct_opened(id, person).map_err(|err| Fault::Net(err.to_string()))?;
        Ok(id.to_owned())
    }

    /// Who the user has direct conversations with, as the last sidebar load left it: read from
    /// its note once a run, so a connection that does not load the sidebar still knows.
    pub(super) fn partners(&self) {
        if std::mem::replace(&mut self.state().partners_read, true) {
            return;
        }
        let kept = self.ctx.note("partners").unwrap_or_default();
        let pairs = kept.split(',').filter_map(|pair| pair.split_once('='));
        let mut state = self.state();
        for (person, channel) in pairs {
            state.partners.entry(person.to_owned()).or_insert_with(|| channel.to_owned());
        }
    }

    pub(super) fn keep_partners(&self) {
        let state = self.state();
        let mut pairs: Vec<String> = state.partners.iter().map(|(person, channel)| format!("{person}={channel}")).collect();
        drop(state);
        pairs.sort_unstable();
        self.ctx.set_note("partners", &pairs.join(","));
    }

    /// Records a direct conversation and its partner as someone I talk to.
    pub(super) fn direct_opened(&self, channel: &str, person: &str) -> rusqlite::Result<()> {
        let row = self.direct_row(channel, person);
        self.ctx.conversation(&row)?;
        self.state().partners.insert(person.to_owned(), channel.to_owned());
        self.keep_partners();
        self.ctx.relation(person, Relation::Contact)
    }

    /// The row of a direct conversation. The partner is looked up first, which registers
    /// their picture and records them before the row that names both.
    pub(super) fn direct_row(&self, channel: &str, person: &str) -> ConversationRow {
        let user = self.user(person);
        let mut row = self.placed(channel, protocol::ConversationKind::Direct, &user.name);
        row.group = self.ctx.note("group:directs");
        row.avatar = Some(user.avatar).filter(|avatar| !avatar.is_empty());
        row.peer = Some(person.to_owned());
        row.private = true;
        // An app's DM, Slackbot's included: never a person (rule 4).
        row.bot = user.bot;
        row.mark = user.mark;
        row
    }

    /// `user_change`: a profile changed. Only people already seen are kept up to date; the rest
    /// of the directory is none of our business until they turn up.
    pub(super) fn user_changed(&self, user: &Value) -> Result<(), Failure> {
        let Some(id) = user["id"].as_str() else {
            return Ok(());
        };
        let Some(before) = self.cached(id) else {
            return Ok(());
        };
        let learned = self.learn(std::slice::from_ref(user));
        let Some((_, after)) = learned.first() else {
            return Ok(());
        };
        let channel = self.state().partners.get(id).cloned();
        if let Some(channel) = channel.filter(|_| before.name != after.name || before.avatar != after.avatar) {
            if let Some(known) = self.ctx.known(&channel)? {
                let mut row = self.direct_row(&channel, id);
                row.group = known.group;
                row.position = known.position;
                self.ctx.conversation(&row)?;
            }
        }
        Ok(())
    }

    /// The status emoji and huddle of everyone the user has a DM with, from what is cached: chatd
    /// keeps them only for owned people, which partners are once the sidebar says so, and
    /// forgets them when the socket drops. Asks Slack nothing.
    pub(super) fn tell_partners(&self) {
        let partners: Vec<String> = self.state().partners.keys().cloned().collect();
        for id in partners {
            if let Some(user) = self.cached(&id) {
                self.ctx.activity(&id, &user.emoji, huddle(user.huddle), true);
            }
        }
    }

    /// Whether a person is on a list the user owns: someone they talk to, or themselves.
    pub(super) fn owned(&self, id: &str) -> bool {
        self.me.get().is_some_and(|me| me == id) || self.state().partners.contains_key(id)
    }

    /// `presence_change`, for one person or a batch. Away is all Slack says of someone not
    /// active, so it is drawn as offline.
    pub(super) fn presence_changed(&self, event: &Value) {
        let presence = match event["presence"].as_str() {
            Some("active") => Presence::Online,
            Some("away") => Presence::Offline,
            _ => return,
        };
        let one = event["user"].as_str().into_iter();
        let many = event["users"].as_array().into_iter().flatten().filter_map(Value::as_str);
        for id in one.chain(many) {
            let status = self.cached(id).map(|user| user.status).unwrap_or_default();
            self.ctx.presence(id, presence, &status, self.owned(id));
        }
    }

    /// `manual_presence_change`: the user set themselves away or back, here or elsewhere.
    pub(super) fn own_presence(&self, word: &str) {
        let Some(me) = self.me.get() else {
            return;
        };
        self.presence_changed(&json!({"user": me, "presence": word}));
    }

    /// Subscribes the socket to the presence of exactly the people the shell has on screen,
    /// and to our own, which the account shows. `presence_sub` replaces the list;
    /// `presence_query` asks for the newcomers' state now.
    pub(super) fn watch(&self) {
        let watching = self.ctx.watching();
        let mine = watching.people.iter().filter(|person| person.account() == self.ctx.account);
        let mut ids: Vec<String> = mine.map(|person| person.id.clone()).chain(self.me.get().cloned()).collect();
        ids.sort();
        ids.dedup();
        let mut state = self.state();
        if state.watched == ids {
            return;
        }
        let new: Vec<&String> = ids.iter().filter(|id| !state.watched.contains(id)).collect();
        let mut frames = vec![json!({"type": "presence_sub", "ids": ids})];
        frames.extend(new.chunks(100).map(|batch| json!({"type": "presence_query", "ids": batch})));
        for frame in frames {
            state.queue(frame);
        }
        state.watched = ids;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn someone_with_no_picture_of_their_own_gets_slacks_default_and_gravatar_is_never_asked() {
        let profile = "https://secure.gravatar.com/avatar/c8d4.jpg?s=72&d=https%3A%2F%2Fa.slack-edge.com%2Fdf10d%2Fimg%2Favatars%2Fava_0002-72.png";
        assert_eq!(gravatar_default(profile).as_deref(), Some("https://a.slack-edge.com/df10d/img/avatars/ava_0002-72.png"));
        // No fallback named, or not gravatar at all: nothing to register.
        assert_eq!(gravatar_default("https://secure.gravatar.com/avatar/c8d4.jpg?s=72"), None);
        assert_eq!(gravatar_default("https://example.org/a.png?d=https%3A%2F%2Fa.slack-edge.com%2Fa.png"), None);
        assert_eq!(gravatar_default("https://gravatar.com.example.org/a?d=https%3A%2F%2Fa.slack-edge.com%2Fa.png"), None);
    }

    #[test]
    fn a_note_in_an_older_form_reads_as_one_and_a_new_one_keeps_where_the_picture_is() {
        let old: Kept = serde_json::from_str(r#"{"n":"Ana","h":"ana","a":"avatar:U1:abc"}"#).unwrap();
        assert_eq!((old.form, old.picture.as_str()), (0, ""));
        let kept = Kept {
            user: old.user,
            form: NOTE,
            picture: "https://avatars.slack-edge.com/a_72.png".to_owned(),
            large: String::new(),
        };
        let read: Kept = serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
        assert_eq!((read.form, read.user.name.as_str(), read.picture.as_str()), (NOTE, "Ana", kept.picture.as_str()));
    }
}
