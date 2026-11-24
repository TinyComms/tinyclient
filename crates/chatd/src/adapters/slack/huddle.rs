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
//! Nothing is ever asked of Slack for a huddle. A page that is read anyway says what is running,
//! and the socket says what changes.

use super::people::User;
use super::{message, Slack, State};
use protocol::Occupant;
use serde_json::Value;
use std::collections::HashSet;

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
    /// Of one entry of `participants`: the person's id, when the entry is not the id itself.
    pub const ID: &str = "id";
    pub const USER_ID: &str = "user_id";
}

/// A huddle running in a conversation: who is in it, in Slack's order, and when it started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Room {
    pub people: Vec<String>,
    /// Unix seconds; 0 when Slack does not say.
    pub started: i64,
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

/// What the room says. A room that names neither who is in it nor an end (Slack leaves both out
/// of some answers) says nothing, and is left alone.
pub(super) fn standing(room: &Value) -> Option<Standing> {
    if !room.is_object() {
        return None;
    }
    if over(room) {
        return Some(Standing::Over);
    }
    let people = room[field::PEOPLE].as_array()?;
    let mut seen = HashSet::new();
    let people = people.iter().filter_map(person).filter(|id| seen.insert(*id)).map(str::to_owned).collect();
    Some(Standing::Running(Room { people, started: room[field::STARTED].as_i64().unwrap_or(0) }))
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
            match standing(room) {
                Some(Standing::Running(room)) => self.room_set(channel, Some(room)),
                Some(Standing::Over) => self.room_set(channel, None),
                None => {}
            }
        }
    }

    /// `sh_room_join`, `sh_room_leave`, `sh_room_update`. With a room, the huddle as it stands
    /// for each conversation it names; without one, the person it names moves in or out.
    pub(super) fn room_event(&self, event: &Value) {
        let room = &event[field::ROOM];
        if room.is_object() {
            let mut channels: Vec<String> = room[field::CHANNELS].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            if channels.is_empty() {
                channels.extend(event[field::CHANNEL].as_str().map(str::to_owned));
            }
            match standing(room) {
                Some(Standing::Running(room)) => {
                    for channel in channels {
                        self.room_set(&channel, Some(room.clone()));
                    }
                }
                Some(Standing::Over) => {
                    for channel in channels {
                        self.room_set(&channel, None);
                    }
                }
                None => {}
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
        if self.ctx.known(channel).ok().flatten().is_none() {
            return;
        }
        let mut state = self.state();
        if state.rooms.get(channel) == room.as_ref() {
            return;
        }
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
