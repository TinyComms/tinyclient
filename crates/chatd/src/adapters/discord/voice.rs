//! Call signalling. The media is mediad's: this joins and leaves on the gateway, hands over the
//! voice session Discord gives out, and reports who is in which voice channel, who is in the
//! call, who is ringing, and when the other side picked up.

use super::model::{self, VoiceState};
use super::rest::Refused;
use super::Discord;
use protocol::{
    CallFailure, CallSupport, ConversationKind, Dave, Network, Occupant, Participant, Relation, VoiceJoin, Watching,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

/// Voice states remembered across every server. A person in a voice channel is one entry.
const HEARD: usize = 2_000;
/// How long Discord has to hand out a voice session.
const PATIENCE: Duration = Duration::from_secs(15);
/// How long our own voice state has to show up in a direct call before the other side is rung.
const RING_AFTER: Duration = Duration::from_secs(5);
/// The flags the web client sends with its voice state. From the community documentation;
/// unconfirmed against a real account.
const FLAGS: u8 = 2;

/// The words of each way a call ends here. No status code or Discord error text is among them.
const NO_ANSWER: &str = "Nobody picked up.";
/// The dock tells a decline from no answer by this sentence.
// CONTRACT: `CallFailure` has one reason, `NoAnswer`, for "declined" and "rang out".
const DECLINED: &str = "They declined the call.";
const REMOVED: &str = "Discord took you out of the call. You were removed, or the channel is gone.";
const NO_SESSION: &str = "No reply from Discord after 10 seconds.";

/// What pressing the call button of a server channel does. A voice channel is joined here. A
/// stage is not: a listener there cannot simply speak, so it opens in Discord.
pub(super) fn support(kind: u8) -> CallSupport {
    match kind {
        model::VOICE => CallSupport::Native,
        model::STAGE => CallSupport::Handoff,
        _ => CallSupport::None,
    }
}

/// Where Discord takes a conversation: its own app when one is installed, else the web.
fn link(guild: Option<&str>, channel: &str, app: bool) -> String {
    let place = format!("channels/{}/{channel}", guild.unwrap_or("@me"));
    if app {
        // The form Discord's own links use to open the app on a channel.
        format!("discord://-/{place}")
    } else {
        format!("https://discord.com/{place}")
    }
}

/// True when some application on this Mac opens `discord://` links. Asks LaunchServices;
/// nothing is launched. Other systems hand off through the browser.
fn app_installed() -> bool {
    #[cfg(not(target_os = "macos"))]
    {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
    app_installed_macos()
    }
}

#[cfg(target_os = "macos")]
fn app_installed_macos() -> bool {
    use std::ffi::c_void;
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn CFURLCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            base: *const c_void,
        ) -> *const c_void;
        fn LSCopyDefaultApplicationURLForURL(url: *const c_void, roles: u32, error: *mut *const c_void) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }
    const UTF8: u32 = 0x0800_0100;
    const ALL_ROLES: u32 = 0xffff_ffff;
    let scheme = b"discord://-/";
    // SAFETY: the bytes outlive the call that copies them; every object made or copied here is
    // released once, and a null answer is not released.
    unsafe {
        let url = CFURLCreateWithBytes(std::ptr::null(), scheme.as_ptr(), scheme.len() as isize, UTF8, std::ptr::null());
        if url.is_null() {
            return false;
        }
        let handler = LSCopyDefaultApplicationURLForURL(url, ALL_ROLES, std::ptr::null_mut());
        CFRelease(url);
        if handler.is_null() {
            return false;
        }
        CFRelease(handler);
        true
    }
}

/// Why the other side was not rung, for the dock. Status codes, Discord's own error text and
/// what the network stack said stay in the log.
fn ring_refused(refused: &Refused) -> String {
    match refused {
        Refused::Network(_) => "Discord couldn't be reached to place the call. Check your connection, then try again.".to_owned(),
        Refused::Status { code: 50013 | 50001 | 50007, .. } | Refused::Status { status: 403, .. } => {
            "Discord didn't let you call here. They may not accept calls from you.".to_owned()
        }
        Refused::Status { code: 40002, .. } | Refused::Expired | Refused::Challenge | Refused::Limited(_) | Refused::Paused(_) => {
            refused.reason()
        }
        Refused::Status { .. } => "Discord didn't place the call. Try again, or call from Discord.".to_owned(),
    }
}

pub(super) fn handoff_link(guild: Option<&str>, channel: &str) -> String {
    link(guild, channel, app_installed())
}

/// Someone in a voice channel or a direct call.
pub struct Heard {
    pub guild: Option<String>,
    channel: String,
    name: String,
    muted: bool,
    deafened: bool,
    streaming: bool,
}

impl Heard {
    pub fn channel(&self) -> &str {
        &self.channel
    }
}

/// The call the user started or accepted, from the request until it is left.
struct Joining {
    conversation: String,
    guild: Option<String>,
    /// From our own `VOICE_STATE_UPDATE`.
    session: Option<String>,
    /// Endpoint and token, from `VOICE_SERVER_UPDATE`.
    server: Option<(String, String)>,
    /// mediad has been given the session as it is now.
    reported: bool,
    since: Instant,
    muted: bool,
    deafened: bool,
    /// We started a direct call, so the other side is rung.
    ring: bool,
    /// Discord showed the other side ringing; when that stops with nobody there, nobody answered.
    saw_ringing: bool,
    /// chatd has been told the other side picked up.
    answered: bool,
}

#[derive(Default)]
pub struct Voice {
    /// By person id, us included while we are in a channel. Feeds the roster of the call we are
    /// in and the occupants of the voice channels on screen.
    pub states: HashMap<String, Heard>,
    call: Option<Joining>,
    /// Direct conversations with a call going on.
    calls: HashSet<String>,
    /// Direct conversations whose call is ringing us.
    ringing: HashSet<String>,
    /// The server whose tree is on screen.
    shown: Option<String>,
    /// Channels chatd holds occupants for, each with its server, so it can be told when they
    /// empty or leave the screen.
    told: HashMap<String, Option<String>>,
}

impl Voice {
    /// A new gateway session: every voice state will be told again.
    pub(super) fn reset(&mut self) {
        self.states.clear();
        self.told.clear();
    }

    /// True for a channel whose occupants chatd is to know: one of the server on screen, the
    /// call we are in, or a direct call in progress.
    fn watched(&self, channel: &str, guild: Option<&str>) -> bool {
        let ours = self.call.as_ref().is_some_and(|call| call.conversation == channel);
        ours || match guild {
            Some(guild) => self.shown.as_deref() == Some(guild),
            None => true,
        }
    }
}

#[derive(Deserialize)]
pub struct ServerUpdate {
    token: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    /// Null while Discord is moving the call to another server.
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Deserialize)]
pub struct CallEvent {
    channel_id: String,
    #[serde(default)]
    ringing: Vec<String>,
    #[serde(default)]
    voice_states: Vec<VoiceState>,
}

fn voice_state(guild: Option<&str>, channel: Option<&str>, muted: bool, deafened: bool) -> Value {
    json!({
        "guild_id": guild,
        "channel_id": channel,
        "self_mute": muted,
        "self_deaf": deafened,
        "self_video": false,
        "flags": FLAGS,
    })
}

impl Discord {
    /// Everyone in the call but us, to chatd, whenever a voice state changed; and, on a call we
    /// rang, the moment someone else is in it: that is the answer.
    fn roster(&self) {
        let (conversation, participants, answered) = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let Some(call) = &mut state.voice.call else {
                return;
            };
            let there = state.voice.states.iter().filter(|(id, heard)| heard.channel == call.conversation && **id != state.me);
            let participants: Vec<Participant> = there
                .map(|(id, heard)| Participant {
                    id: id.clone(),
                    name: heard.name.clone(),
                    speaking: false,
                    muted: heard.muted,
                    deafened: heard.deafened,
                })
                .collect();
            let answered = call.ring && !call.answered && !participants.is_empty();
            call.answered |= answered;
            (call.conversation.clone(), participants, answered)
        };
        self.ctx.participants(&conversation, participants);
        if answered {
            self.ctx.answered(&conversation);
        }
    }

    /// Tells chatd who is in each of `channels`, for those it is to know (see
    /// [`Voice::watched`]): at most a row's worth of people, friends first then in name order,
    /// us included, and the true count. A channel that emptied is told once.
    fn occupancy(&self, channels: BTreeSet<String>) {
        let mut reports = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            for channel in channels {
                let mut there: Vec<(&String, &Heard)> = state.voice.states.iter().filter(|(_, heard)| heard.channel == channel).collect();
                let guild = there.first().map(|(_, heard)| heard.guild.clone()).or_else(|| state.voice.told.get(&channel).cloned());
                let Some(guild) = guild else {
                    continue;
                };
                if there.is_empty() {
                    if state.voice.told.remove(&channel).is_some() {
                        reports.push((channel, Vec::new(), 0));
                    }
                    continue;
                }
                // A channel with a friend in it is told wherever it is: the home's "In voice".
                let friend = there.iter().any(|(id, _)| state.relations.get(*id) == Some(&Relation::Friend));
                if !friend && !state.voice.watched(&channel, guild.as_deref()) {
                    // No longer on screen and not our call any more: chatd lets it go.
                    if state.voice.told.remove(&channel).is_some() {
                        reports.push((channel, Vec::new(), 0));
                    }
                    continue;
                }
                // Friends first (`02` §4), then by name, so the cap never hides a friend.
                there.sort_by_cached_key(|(id, heard)| (state.relations.get(*id) != Some(&Relation::Friend), heard.name.to_lowercase(), (*id).clone()));
                let total = there.len() as u32;
                let people = there.into_iter().take(protocol::OCCUPANTS_LIMIT).map(|(id, heard)| Occupant {
                    person: self.ctx.key(id),
                    name: heard.name.clone(),
                    muted: heard.muted,
                    deafened: heard.deafened,
                    streaming: heard.streaming,
                    speaking: false,
                });
                let people: Vec<Occupant> = people.collect();
                state.voice.told.insert(channel.clone(), guild);
                reports.push((channel, people, total));
            }
        }
        for (channel, people, total) in reports {
            self.ctx.occupants(&channel, people, total);
        }
    }

    /// What the shell has on screen changed. The voice channels of the server it shows are
    /// told whole; those of the server it left are let go, so chatd holds occupants only for
    /// what can be seen (and for the call we are in, wherever it is).
    pub(super) fn voice_watch(&self, watching: &Watching) {
        let shown = watching.space.as_ref().filter(|space| space.network == Network::Discord && space.account == self.ctx.account.account);
        let shown = shown.map(|space| space.id.clone());
        let (leave, enter) = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let voice = &mut state.voice;
            if voice.shown == shown {
                return;
            }
            let before = std::mem::replace(&mut voice.shown, shown.clone());
            let ours = voice.call.as_ref().map(|call| call.conversation.clone());
            // A friend's channel stays told: it is not the server on screen that wants it.
            let friended: BTreeSet<&String> = voice
                .states
                .iter()
                .filter(|(id, _)| state.relations.get(*id) == Some(&Relation::Friend))
                .map(|(_, heard)| &heard.channel)
                .collect();
            let left = voice.told.iter().filter(|(channel, guild)| {
                guild.is_some() && **guild == before && Some(*channel) != ours.as_ref() && !friended.contains(channel)
            });
            let leave: Vec<String> = left.map(|(channel, _)| channel.clone()).collect();
            for channel in &leave {
                voice.told.remove(channel);
            }
            let entered = voice.states.values().filter(|heard| shown.is_some() && heard.guild == shown);
            let enter: BTreeSet<String> = entered.map(|heard| heard.channel.clone()).collect();
            (leave, enter)
        };
        for channel in leave {
            self.ctx.occupants(&channel, Vec::new(), 0);
        }
        self.occupancy(enter);
    }

    /// Voice states as Discord lists them: a server's at connect, one person's as it changes.
    pub(super) fn voice_states(&self, guild: Option<&str>, states: &[VoiceState]) {
        if states.is_empty() {
            return;
        }
        let mut changed = BTreeSet::new();
        {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            for voice in states {
                let before = state.voice.states.remove(&voice.user_id);
                changed.extend(before.as_ref().map(|before| before.channel.clone()));
                let Some(channel) = &voice.channel_id else {
                    continue;
                };
                let in_call = state.voice.call.as_ref().is_some_and(|call| call.conversation == *channel);
                let kept = before.is_some() || in_call || voice.user_id == state.me;
                if state.voice.states.len() >= HEARD && !kept {
                    continue;
                }
                let named = voice.member.as_ref().map(|member| member.name().to_owned()).filter(|name| !name.is_empty());
                let named = named.or_else(|| state.users.get(&voice.user_id).cloned());
                let me = (voice.user_id == state.me).then(|| "You".to_owned());
                let heard = Heard {
                    guild: voice.guild_id.clone().or_else(|| guild.map(str::to_owned)),
                    channel: channel.clone(),
                    name: named.or(before.map(|before| before.name)).or(me).unwrap_or_default(),
                    muted: voice.self_mute || voice.mute,
                    deafened: voice.self_deaf || voice.deaf,
                    streaming: voice.self_stream || voice.self_video,
                };
                changed.insert(channel.clone());
                state.voice.states.insert(voice.user_id.clone(), heard);
            }
        }
        self.roster();
        self.occupancy(changed);
        self.friends_moved(states.iter().map(|voice| voice.user_id.as_str()));
    }

    pub(super) fn voice_left(&self, people: &[String]) {
        let mut changed = BTreeSet::new();
        {
            let mut state = self.state.lock().unwrap();
            for person in people {
                changed.extend(state.voice.states.remove(person).map(|heard| heard.channel));
            }
        }
        self.roster();
        self.occupancy(changed);
        self.friends_moved(people.iter().map(String::as_str));
    }

    /// Friends whose voice state changed: what they are doing is told again.
    fn friends_moved<'a>(&self, people: impl Iterator<Item = &'a str>) {
        let friends: Vec<String> = {
            let state = self.state.lock().unwrap();
            let friend = |id: &&str| state.relations.get(*id) == Some(&Relation::Friend);
            people.filter(friend).map(str::to_owned).collect()
        };
        for friend in friends {
            self.tell_activity(&friend);
        }
    }

    /// Hands mediad the voice session once both halves of it are here.
    fn report(&self) {
        let join = {
            let mut state = self.state.lock().unwrap();
            let me = state.me.clone();
            let Some(call) = state.voice.call.as_mut().filter(|call| !call.reported) else {
                return;
            };
            let (Some(session), Some((endpoint, token))) = (&call.session, &call.server) else {
                return;
            };
            call.reported = true;
            let join = VoiceJoin {
                network: Network::Discord,
                endpoint: endpoint.clone(),
                token: token.clone(),
                session_id: session.clone(),
                user_id: me,
                // A direct call's voice server is named by its channel.
                server_id: call.guild.clone().unwrap_or_else(|| call.conversation.clone()),
                channel_id: call.conversation.clone(),
                // Discord requires its end-to-end protocol on every call; version 1 is the one
                // it has published. mediad offers what it speaks, up to this.
                dave: Dave {
                    max_version: 1,
                    required: true,
                },
                muted: call.muted,
                deafened: call.deafened,
            };
            (call.conversation.clone(), join)
        };
        self.ctx.voice(&join.0, join.1);
    }

    pub(super) fn voice_state(&self, voice: VoiceState) {
        // Ours too: we are among the occupants of the channel we are in, from any device.
        self.voice_states(voice.guild_id.as_deref(), std::slice::from_ref(&voice));
        if voice.user_id != self.me() {
            return;
        }
        let dropped = {
            let mut state = self.state.lock().unwrap();
            let Some(call) = &mut state.voice.call else {
                return;
            };
            match &voice.channel_id {
                Some(channel) if *channel == call.conversation => {
                    if call.session != voice.session_id {
                        call.session = voice.session_id.clone();
                        call.reported = false;
                    }
                    None
                }
                // Moved elsewhere from another device: not the call the user asked for here.
                Some(_) => None,
                // We were in and Discord took us out: kicked, or the channel is gone.
                None if call.reported => state.voice.call.take().map(|call| call.conversation),
                None => None,
            }
        };
        match dropped {
            Some(conversation) => self.ctx.call_failed(&conversation, CallFailure::Refused, REMOVED),
            None => self.report(),
        }
    }

    pub(super) fn voice_server(&self, update: ServerUpdate) {
        {
            let mut state = self.state.lock().unwrap();
            let Some(call) = &mut state.voice.call else {
                return;
            };
            let ours = match (&update.guild_id, &call.guild) {
                (Some(theirs), Some(ours)) => theirs == ours,
                (None, None) => update.channel_id.as_ref().is_none_or(|channel| *channel == call.conversation),
                _ => false,
            };
            let Some(endpoint) = update.endpoint.filter(|_| ours) else {
                return;
            };
            call.server = Some((endpoint, update.token));
            call.reported = false;
        }
        self.report();
    }

    /// Joins a voice channel, or a direct conversation's call. Starting a direct call also rings
    /// the other side, once Discord has us in it.
    pub(super) fn call_join(&self, conversation_id: &str) {
        let refuse = |why: &str| self.ctx.call_failed(conversation_id, CallFailure::Refused, why);
        let Ok(Some(conversation)) = self.ctx.known(conversation_id) else {
            return refuse("This conversation is gone.");
        };
        let direct = conversation.kind.is_direct();
        if conversation.call != CallSupport::Native || (!direct && conversation.kind != ConversationKind::Voice) {
            return refuse("This conversation has no call tinyclient can join.");
        }
        let guild = conversation.space.clone();
        let ring = {
            let mut state = self.state.lock().unwrap();
            let ring = direct && !state.voice.calls.contains(conversation_id);
            state.voice.call = Some(Joining {
                conversation: conversation_id.to_owned(),
                guild: guild.clone(),
                session: None,
                server: None,
                reported: false,
                since: Instant::now(),
                muted: false,
                deafened: false,
                ring,
                saw_ringing: false,
                answered: false,
            });
            ring
        };
        // Said before anything can come back: our own voice connection may be up before the
        // ring is even placed, and until someone answers that is not a call.
        if ring {
            self.ctx.ringing_out(conversation_id);
        }
        self.tell(4, voice_state(guild.as_deref(), Some(conversation_id), false, false));
        // Whoever is already there, before anyone's state changes.
        self.roster();
        self.occupancy(BTreeSet::from([conversation_id.to_owned()]));
        if !ring {
            return;
        }
        // The call exists once our own voice state is in it; ringing before that is refused.
        let joined = || {
            let state = self.state.lock().unwrap();
            state.voice.call.as_ref().filter(|call| call.conversation == conversation_id).map(|call| call.session.is_some())
        };
        let deadline = Instant::now() + RING_AFTER;
        while joined() == Some(false) && Instant::now() < deadline {
            if self.ctx.pause(Duration::from_millis(50)) {
                return;
            }
        }
        if joined() != Some(true) {
            return;
        }
        let rung = self.call("POST", &format!("/channels/{conversation_id}/call/ring"), Some(json!({"recipients": null})));
        if let Err(refused) = rung {
            self.log("ring", &refused);
            self.call_leave(conversation_id);
            refuse(&ring_refused(&refused));
        }
    }

    /// Leaves the call we are in, or declines the one ringing us. For a call already left (it
    /// failed, and chatd says so too) this is nothing.
    pub(super) fn call_leave(&self, conversation_id: &str) {
        let (was, ringing) = {
            let mut state = self.state.lock().unwrap();
            let ours = state.voice.call.as_ref().is_some_and(|call| call.conversation == conversation_id);
            let was = if ours { state.voice.call.take() } else { None };
            (was, state.voice.ringing.remove(conversation_id))
        };
        match was {
            Some(call) => self.tell(4, voice_state(call.guild.as_deref(), None, false, false)),
            None if ringing => {
                let path = format!("/channels/{conversation_id}/call/stop-ringing");
                if let Err(refused) = self.call("POST", &path, Some(json!({}))) {
                    self.log("decline", refused);
                }
            }
            None => {}
        }
    }

    pub(super) fn call_audio(&self, muted: bool, deafened: bool) {
        let frame = {
            let mut state = self.state.lock().unwrap();
            let Some(call) = &mut state.voice.call else {
                return;
            };
            (call.muted, call.deafened) = (muted, deafened);
            voice_state(call.guild.as_deref(), Some(&call.conversation), muted, deafened)
        };
        self.tell(4, frame);
    }

    /// A direct call began or its ringing changed.
    pub(super) fn call_changed(&self, event: CallEvent) {
        self.state.lock().unwrap().voice.calls.insert(event.channel_id.clone());
        // Whoever joined is the answer to a ring we placed (see `roster`).
        self.voice_states(None, &event.voice_states);
        enum Now {
            Ring(String),
            Quiet,
            Unanswered(Option<String>),
            Nothing,
        }
        let now = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let rings_me = event.ringing.contains(&state.me);
            let others = state.voice.states.iter().any(|(id, heard)| heard.channel == event.channel_id && *id != state.me);
            match state.voice.call.as_mut().filter(|call| call.conversation == event.channel_id) {
                Some(call) if call.ring => {
                    call.saw_ringing |= !event.ringing.is_empty();
                    if call.saw_ringing && event.ringing.is_empty() && !others && !call.answered {
                        Now::Unanswered(state.voice.call.take().and_then(|call| call.guild))
                    } else {
                        Now::Nothing
                    }
                }
                Some(_) => Now::Nothing,
                None if rings_me => {
                    state.voice.ringing.insert(event.channel_id.clone());
                    let caller = event.voice_states.first().map(|voice| voice.user_id.as_str());
                    let named = caller.and_then(|caller| state.users.get(caller)).cloned();
                    Now::Ring(named.unwrap_or_default())
                }
                None => {
                    state.voice.ringing.remove(&event.channel_id);
                    Now::Quiet
                }
            }
        };
        match now {
            Now::Ring(from) => {
                let title = || self.ctx.known(&event.channel_id).ok().flatten().map(|row| row.title);
                let from = if from.is_empty() { title().unwrap_or_default() } else { from };
                // CONTRACT: chatd shows one call. A ring that arrives while we are in another
                // call is dropped there, so the dock cannot offer it.
                self.ctx.ring(&event.channel_id, &from);
            }
            Now::Quiet => self.ctx.ring_stopped(&event.channel_id),
            Now::Unanswered(guild) => {
                self.tell(4, voice_state(guild.as_deref(), None, false, false));
                // Discord stops the ringing itself only when the other side says no; a ring
                // nobody touches goes on until we give up (chatd does, after a minute). With one
                // person on the other side that makes this a decline. Unconfirmed against a
                // real account; in a group it only says that nobody is left to answer.
                let one = self.ctx.known(&event.channel_id).ok().flatten().is_some_and(|row| row.kind == ConversationKind::Direct);
                let words = if one { DECLINED } else { NO_ANSWER };
                self.ctx.call_failed(&event.channel_id, CallFailure::NoAnswer, words);
            }
            Now::Nothing => {}
        }
    }

    pub(super) fn call_deleted(&self, event: CallEvent) {
        {
            let mut state = self.state.lock().unwrap();
            state.voice.calls.remove(&event.channel_id);
            state.voice.ringing.remove(&event.channel_id);
            state.voice.states.retain(|_, heard| heard.channel != event.channel_id);
        }
        self.ctx.ring_stopped(&event.channel_id);
        self.roster();
        self.occupancy(BTreeSet::from([event.channel_id]));
    }

    /// Gives up on a join Discord never answered. True while one is waiting.
    pub(super) fn voice_tick(&self) -> bool {
        let late = {
            let mut state = self.state.lock().unwrap();
            let Some(call) = &state.voice.call else {
                return false;
            };
            if call.reported {
                return false;
            }
            if call.since.elapsed() < PATIENCE {
                return true;
            }
            state.voice.call.take()
        };
        if let Some(call) = late {
            self.tell(4, voice_state(call.guild.as_deref(), None, false, false));
            self.ctx.call_failed(&call.conversation, CallFailure::Lost, NO_SESSION);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_is_never_joined_here_and_a_voice_channel_is() {
        assert_eq!(support(model::VOICE), CallSupport::Native);
        assert_eq!(support(model::STAGE), CallSupport::Handoff, "a stage opens in Discord");
        assert_eq!(support(model::TEXT), CallSupport::None);
        assert_eq!(support(model::FORUM), CallSupport::None);
    }

    #[test]
    fn the_hand_off_opens_the_app_on_the_channel_and_the_web_without_one() {
        assert_eq!(link(Some("81"), "900", true), "discord://-/channels/81/900");
        assert_eq!(link(None, "3001", true), "discord://-/channels/@me/3001");
        assert_eq!(link(Some("81"), "900", false), "https://discord.com/channels/81/900");
        assert_eq!(link(None, "3001", false), "https://discord.com/channels/@me/3001");
        // Whatever this Mac has installed, asking is safe and says one or the other.
        let found = handoff_link(None, "3001");
        assert!(found.ends_with("channels/@me/3001") && (found.starts_with("discord://-/") || found.starts_with("https://discord.com/")));
    }

    #[test]
    fn occupants_are_wanted_for_the_server_on_screen_the_call_and_direct_calls_only() {
        let mut voice = Voice { shown: Some("81".into()), ..Voice::default() };
        assert!(voice.watched("900", Some("81")));
        assert!(!voice.watched("901", Some("82")), "a server that is not on screen");
        assert!(voice.watched("3001", None), "a direct call in progress");
        voice.call = Some(Joining {
            conversation: "901".into(),
            guild: Some("82".into()),
            session: None,
            server: None,
            reported: false,
            since: Instant::now(),
            muted: false,
            deafened: false,
            ring: false,
            saw_ringing: false,
            answered: false,
        });
        assert!(voice.watched("901", Some("82")), "the call we are in, wherever it is");
    }
}
