//! A huddle running in a conversation, and who is in it, so the conversation's row says so the
//! way a voice channel's does.
//!
//! Slack tells it with the message that carries the room: on the socket as the huddle starts and
//! as Slack changes the same message when people come and go or it ends, and on a page of
//! history. The huddle card reads the same object (`message::huddle_note`).
//!
//! It also tells it with events of their own, [`JOIN`], [`LEAVE`] and [`UPDATE`]. Unverified: the
//! three names are the community's and no capture of one was seen. Each is taken to carry the
//! room as the message does; one that carries no room is taken to name a person ([`field::USER`])
//! and the conversation ([`field::CHANNEL`]), moving that one person in or out.
//!
//! Every field name read in a room or an event is a constant of this module, so a capture that
//! corrects one corrects it in one edit.
//!
//! Nothing is asked of Slack to find a huddle. A page that is read anyway says what is running,
//! and the socket says what changes. The people a huddle lists are looked up when their names are
//! not known yet, as any message's authors are.

use super::people::User;
use super::{message, Slack, State};
use protocol::{Activity, ActivityKind, Occupant};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// The events, by `type`: someone joins a huddle, leaves it, or it changes.
pub(super) const JOIN: &str = "sh_room_join";
pub(super) const LEAVE: &str = "sh_room_leave";
pub(super) const UPDATE: &str = "sh_room_update";

/// Fields of the room object that a huddle's message carries, and of an event that carries one.
pub(super) mod field {
    pub const ROOM: &str = "room";
    /// Who is in the huddle now.
    pub const PEOPLE: &str = "participants";
    /// The conversations the huddle is in.
    pub const CHANNELS: &str = "channels";
    /// Of an event that names one person and the conversation instead of a room.
    pub const CHANNEL: &str = "channel";
    pub const USER: &str = "user";
    pub const STARTED: &str = "date_start";
    pub const ENDED: &str = "date_end";
    pub const OVER: &str = "has_ended";
    pub const FAMILY: &str = "call_family";
    /// Who was ever in it: the card's faces once the active list is empty.
    pub const EVER: &str = "participant_history";
    pub const CREATOR: &str = "created_by";
    /// Of a huddle rung in a direct conversation, and missed there.
    pub const DIRECT: &str = "is_dm_call";
    pub const MISSED: &str = "was_missed";
    /// The huddle's message, by which huddles of one conversation are told apart.
    pub const MESSAGE: &str = "ts";
    pub const ROOT: &str = "thread_root_ts";
    /// Of one entry of `participants`: the person's id, when the entry is not the id itself.
    pub const ID: &str = "id";
    pub const USER_ID: &str = "user_id";
}

/// A huddle running in a conversation: who is in it, in Slack's order, when it started, and the
/// message that carries it. A conversation has one huddle at a time and one message per huddle,
/// so the message says which huddle is meant: an older message is about one that is over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Room {
    pub people: Vec<String>,
    /// Unix seconds; 0 when Slack does not say.
    pub started: i64,
    /// The huddle's message (`ts`); empty when what said so named none.
    pub message: String,
}

/// What a room object says of the huddle in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Standing {
    Running(Room),
    Over,
}

/// A person's id as a room lists them: the id itself, or an object that holds it.
pub(super) fn person(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| [&value[field::USER_ID], &value[field::ID]].into_iter().find_map(Value::as_str)).filter(|id| !id.is_empty())
}

/// The card's rule for a huddle that is over: Slack said so, or gave the time it ended.
pub(super) fn over(room: &Value) -> bool {
    room[field::OVER] == true || room[field::ENDED].as_i64().is_some_and(|end| end > 0)
}

/// Whether the huddle's message `ts` is older than `than`: by Slack's timestamp, seconds first
/// and then the rest, which is fine for two messages of one second.
fn earlier(ts: &str, than: &str) -> bool {
    (super::seconds(ts), ts) < (super::seconds(than), than)
}

/// Conversations remembered as running a huddle, and people in each. Past either, what arrives
/// later is not kept.
pub(super) const ROOMS: usize = 64;
pub(super) const PEOPLE: usize = protocol::OCCUPANTS_LIMIT;

/// The longest id taken for a conversation, a person or a message. Slack's are 9 to 12
/// characters; a thread's adds its message's timestamp.
const ID: usize = 32;

/// How far back a huddle is believed to have started, for saying how long it has run.
const DAY: i64 = 86_400;

/// Whether something that calls itself an id may be one. Slack's are letters and digits, and a
/// thread's has a colon and a dot. Nothing else is handed on as a key.
fn id_like(id: &str) -> bool {
    !id.is_empty() && id.len() <= ID && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-:".contains(&byte))
}

/// The ids a list of the room names, be they its people or its conversations: at most `cap`,
/// once each, in Slack's order.
fn listed(value: &Value, cap: usize) -> Vec<String> {
    let (mut ids, mut seen) = (Vec::new(), HashSet::new());
    for entry in value.as_array().into_iter().flatten() {
        if ids.len() >= cap {
            break;
        }
        let Some(id) = person(entry).filter(|id| id_like(id)) else {
            continue;
        };
        if seen.insert(id) {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// The huddle's message a room names, for telling huddles of one conversation apart.
fn message_of(room: &Value) -> Option<&str> {
    room[field::ROOT].as_str().filter(|root| id_like(root))
}

/// What the room says. A room that names neither who is in it nor an end (Slack leaves both out
/// of some answers) says nothing, and is left alone.
pub(super) fn standing(room: &Value) -> Option<Standing> {
    if !room.is_object() {
        return None;
    }
    if over(room) {
        return Some(Standing::Over);
    }
    if !room[field::PEOPLE].is_array() {
        return None;
    }
    Some(Standing::Running(Room { people: listed(&room[field::PEOPLE], PEOPLE), started: room[field::STARTED].as_i64().unwrap_or(0), message: String::new() }))
}

impl Slack {
    /// The rooms that messages of `channel` carry: a page of its history, the huddle's own
    /// message as it arrives, and the same message as Slack changes it.
    pub(super) fn rooms_read(&self, channel: &str, entries: &[Value]) {
        for entry in entries {
            let room = &entry[field::ROOM];
            if !message::is_huddle(entry["subtype"].as_str(), room) {
                continue;
            }
            self.room_heard(channel, entry[field::MESSAGE].as_str(), standing(room));
        }
    }

    /// What was said of the huddle in `channel`, with the huddle's message named when it is
    /// known. A conversation keeps one message per huddle, so a message older than the one the
    /// huddle known here was heard from is about a huddle that ran before it, and is left alone:
    /// a page of history holds those of every huddle that is over.
    fn room_heard(&self, channel: &str, message: Option<&str>, said: Option<Standing>) {
        let before = self.state().rooms.get(channel).cloned();
        if let (Some(known), Some(message)) = (&before, message) {
            if !known.message.is_empty() && earlier(message, &known.message) {
                return;
            }
        }
        let after = match said {
            Some(Standing::Running(mut room)) => {
                room.message = message.unwrap_or_default().to_owned();
                Some(room)
            }
            Some(Standing::Over) => None,
            None => return,
        };
        self.room_set(channel, after);
    }

    /// A new connection: nothing heard before the socket dropped says a huddle is running
    /// now. Every row that listed one is emptied; Slack's next word, or the next page read,
    /// says what is running.
    pub(super) fn rooms_forgotten(&self) {
        let mut state = self.state();
        let (mut forgotten, mut moved): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for (channel, room) in state.rooms.drain() {
            moved.extend(room.people);
            forgotten.push(channel);
        }
        for channel in &forgotten {
            self.room_told(channel, &state);
        }
        self.huddles_told(&moved, &state);
    }

    /// "In a huddle" for someone: as their profile says, or because a huddle lists them. With the
    /// conversation it is in, and when it started, where a huddle does, so that "In a huddle" on
    /// them opens that conversation.
    pub(super) fn huddling(&self, id: &str, user: &User) -> Option<Activity> {
        let state = self.state();
        self.huddling_in(id, user, &state.rooms)
    }

    fn huddling_in(&self, id: &str, user: &User, rooms: &HashMap<String, Room>) -> Option<Activity> {
        let sat_in = rooms.iter().find_map(|(channel, room)| room.people.iter().any(|who| who == id).then_some((channel.as_str(), room.started)));
        let Some((channel, started)) = sat_in else {
            return user.huddle.then(|| Activity::new(ActivityKind::Huddle, ""));
        };
        let mut activity = Activity::new(ActivityKind::Huddle, self.ctx.known(channel).ok().flatten().map_or_else(String::new, |row| row.title));
        activity.conversation = Some(self.ctx.key(channel));
        // A start further back than a day is not believed: a huddle is not run that long.
        activity.started = (started > 0 && crate::hub::now().saturating_sub(started) <= DAY).then_some(started);
        Some(activity)
    }

    /// "In a huddle" said again for those who may have moved between a huddle and none.
    fn huddles_told(&self, moved: &[String], state: &State) {
        let mine = self.me.get().map(String::as_str);
        for id in moved {
            let Some(user) = state.users.get(id) else {
                continue;
            };
            let owned = mine == Some(id.as_str()) || state.partners.contains_key(id);
            self.ctx.activity(id, &user.emoji, self.huddling_in(id, user, &state.rooms), owned);
        }
    }

    /// `sh_room_join`, `sh_room_leave`, `sh_room_update`. With a room, the huddle as it stands
    /// for each conversation it names; without one, the person it names moves in or out.
    pub(super) fn room_event(&self, event: &Value) {
        let room = &event[field::ROOM];
        if room.is_object() {
            let mut channels = listed(&room[field::CHANNELS], ROOMS);
            if channels.is_empty() {
                channels.extend(event[field::CHANNEL].as_str().filter(|channel| id_like(channel)).map(str::to_owned));
            }
            for channel in channels {
                self.room_heard(&channel, message_of(room), standing(room));
            }
            return;
        }
        let (Some(id), Some(channel)) = (person(&event[field::USER]), event[field::CHANNEL].as_str()) else {
            return;
        };
        let joining = event["type"] != LEAVE;
        let before = self.state().rooms.get(channel).cloned();
        let after = if joining {
            let mut room = before.unwrap_or_default();
            if !room.people.iter().any(|who| who == id) {
                room.people.push(id.to_owned());
            }
            Some(room)
        } else {
            before.map(|mut room| {
                room.people.retain(|who| who != id);
                room
            })
        };
        let Some(after) = after else { return };
        self.room_set(channel, Some(after));
    }

    /// The huddle in `channel` as it stands: `None` when there is none. A huddle nobody is in
    /// is none. The row is told, and the channel remembered until the huddle is over.
    fn room_set(&self, channel: &str, room: Option<Room>) {
        let room = room.filter(|room| !room.people.is_empty());
        // A conversation the store has no row for has nothing that could show it.
        if !id_like(channel) || self.ctx.known(channel).ok().flatten().is_none() {
            return;
        }
        let mut state = self.state();
        let before = state.rooms.get(channel).cloned();
        if before == room || (before.is_none() && room.is_some() && state.rooms.len() >= ROOMS) {
            return;
        }
        let mut moved: Vec<String> = before.iter().chain(room.iter()).flat_map(|room| room.people.iter()).cloned().collect();
        moved.sort();
        moved.dedup();
        let people: Vec<String> = room.iter().flat_map(|room| room.people.iter()).cloned().collect();
        match &room {
            Some(room) => {
                state.rooms.insert(channel.to_owned(), room.clone());
            }
            None => {
                state.rooms.remove(channel);
            }
        }
        drop(state);
        // Their names and faces for the row, asked of Slack as any message's authors are.
        self.users(&people);
        let state = self.state();
        self.room_told(channel, &state);
        self.huddles_told(&moved, &state);
    }

    /// Who is in the huddle in `channel`, for its row, as remembered now. Someone not yet named
    /// goes by their id, as the sender of a message does.
    fn room_told(&self, channel: &str, state: &State) {
        let people: Vec<Occupant> = state
            .rooms
            .get(channel)
            .into_iter()
            .flat_map(|room| &room.people)
            .map(|id| {
                let user = state.users.get(id);
                Occupant {
                    person: self.ctx.key(id),
                    name: user.map_or(id.as_str(), |user| user.name.as_str()).to_owned(),
                    avatar: user.and_then(User::avatar),
                    muted: false,
                    deafened: false,
                    server_muted: false,
                    server_deafened: false,
                    suppressed: false,
                    streaming: false,
                    camera: false,
                    speaking: false,
                    hand_raised: false,
                    is_channel: false,
                }
            })
            .collect();
        let total = people.len() as u32;
        self.ctx.occupants(channel, people, total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn running(room: serde_json::Value) -> Room {
        let Standing::Running(room) = standing(&room).expect("a huddle running") else { unreachable!() };
        room
    }

    /// A room object is whatever the network sent. What is kept of it is bounded, and only
    /// shaped like Slack's ids.
    #[test]
    fn a_room_from_the_network_is_capped_and_holds_only_what_looks_like_slacks_ids() {
        let crowd = (0..500).map(|at| format!("U{at:08}")).collect::<Vec<_>>();
        let room = running(json!({"participants": crowd, "date_start": 5}));
        assert_eq!(room.people.len(), PEOPLE, "a crowd is cut to what a row can list");
        assert_eq!(room.people.first().map(String::as_str), Some("U00000000"), "the first heard, in Slack's order");
        assert_eq!(room.people.last(), Some(&format!("U{:08}", PEOPLE - 1)));
        assert_eq!(room.started, 5);

        let odd = json!({"participants": [
            "U0ANA", "U0ANA", {"user_id": "U0BEN"}, {"id": "U0BEN"}, 7, null, [], "", "U 0BEN", "U0BEN\n",
            "<script>", "x".repeat(200), "../..", "U0BOT",
        ]});
        assert_eq!(running(odd).people, ["U0ANA", "U0BEN", "U0BOT"], "once each, by id, and nothing that is not an id");

        let channels = (0..5_000).map(|at| format!("C{at:08}")).collect::<Vec<_>>();
        let room = json!({"participants": ["U0ANA"], "channels": channels, "thread_root_ts": "x".repeat(200)});
        assert_eq!(listed(&room[field::CHANNELS], ROOMS).len(), ROOMS, "a room names a bounded number of conversations");
        assert_eq!(listed(&json!(["C0GEN", "C0GEN", 3, "", "C 0 X", "C0GEN:1760000800.000800"]), ROOMS), ["C0GEN", "C0GEN:1760000800.000800"]);
        // A timestamp as long as that is not one: the huddle is taken as one that named none.
        assert_eq!(message_of(&room), None);
        assert_eq!(message_of(&json!({"thread_root_ts": "1760000800.000800"})), Some("1760000800.000800"));
    }
}
