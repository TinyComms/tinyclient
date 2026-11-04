//! Call signalling. The media is mediad's: this joins and leaves on the gateway, hands over the
//! voice session Discord gives out, and reports who is in which voice channel, who is in the
//! call, who is ringing, and when the other side picked up.

use super::super::{Failure, Picture};
use super::model::{self, VoiceState};
use super::rest::Refused;
use super::Discord;
use protocol::{
    CallFailure, CallSupport, ConversationKind, Dave, MediaRequest, Network, Occupant, Participant, Relation, VoiceJoin, Watching,
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
    /// On a stage's platform (`suppress` false there): who the channel hears. A voice
    /// channel's states never have it true, whatever `suppress` says there.
    live: bool,
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
    /// Our camera is on. Discord is told with the voice state, and mediad tells it again on the
    /// voice gateway once a video SSRC exists.
    video: bool,
}

/// A Go Live leg we have asked for and not yet given up. The voice call stays up beside it.
struct StreamSlot {
    key: String,
    rtc_server: Option<String>,
    rtc_channel: Option<String>,
    token: Option<String>,
    endpoint: Option<String>,
    /// mediad has this session. A new endpoint clears it so the leg connects again.
    told: bool,
    /// The user turned it off before Discord named the stream. The create is then deleted.
    drop: bool,
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
    /// The screen we are sending, once we asked Discord for it.
    share: Option<StreamSlot>,
    /// The share we are watching.
    watch: Option<StreamSlot>,
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

fn voice_state(guild: Option<&str>, channel: Option<&str>, muted: bool, deafened: bool, video: bool) -> Value {
    json!({
        "guild_id": guild,
        "channel_id": channel,
        "self_mute": muted,
        "self_deaf": deafened,
        "self_video": video,
        "flags": FLAGS,
    })
}

/// `guild:guild:channel:user` in a server, `call:channel:user` in a direct call.
fn stream_key(guild: Option<&str>, channel: &str, user: &str) -> String {
    match guild {
        Some(guild) => format!("guild:{guild}:{channel}:{user}"),
        None => format!("call:{channel}:{user}"),
    }
}

fn owned_by(key: &str, user: &str) -> bool {
    key.rsplit(':').next() == Some(user)
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
                    ..Participant::default()
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
    /// [`Voice::watched`]): at most a row's worth of people, on-stage (live) first then
    /// friends then in name order, us included, and the true count. A channel that emptied
    /// is told once.
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
                // On-stage (live) people first, then friends, then by name, so the capped
                // list never hides a speaker or a friend behind a crowd whose names sort earlier.
                there.sort_by_cached_key(|(id, heard)| {
                    (!heard.live, state.relations.get(*id) != Some(&Relation::Friend), heard.name.to_lowercase(), (*id).clone())
                });
                let total = there.len() as u32;
                let people = there.into_iter().take(protocol::OCCUPANTS_LIMIT).map(|(id, heard)| Occupant {
                    person: self.ctx.key(id),
                    name: heard.name.clone(),
                    muted: heard.muted,
                    deafened: heard.deafened,
                    streaming: heard.streaming,
                    speaking: heard.live,
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
        // The stages among the channels touched: `suppress` there picks the audience from the
        // platform. In a voice channel it is Discord's server mute instead, and nobody in one
        // is ever counted as on stage.
        let touched: HashSet<&str> = states.iter().filter_map(|voice| voice.channel_id.as_deref()).collect();
        let stages: HashSet<&str> = touched
            .into_iter()
            .filter(|channel| self.ctx.known(channel).ok().flatten().is_some_and(|row| row.call == CallSupport::Handoff))
            .collect();
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
                let me = (voice.user_id == state.me).then_some("You".to_owned());
                let heard = Heard {
                    guild: voice.guild_id.clone().or_else(|| guild.map(str::to_owned)),
                    channel: channel.clone(),
                    name: named.or(before.map(|before| before.name)).or(me).unwrap_or_default(),
                    muted: voice.self_mute || voice.mute,
                    deafened: voice.self_deaf || voice.deaf,
                    streaming: voice.self_stream || voice.self_video,
                    live: stages.contains(channel.as_str()) && !voice.suppress,
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
                stream: false,
                watch: false,
                group_id: String::new(),
            };
            (call.conversation.clone(), join)
        };
        self.ctx.voice(&join.0, join.1);
        // A stream's events can arrive before this session id does.
        self.flush_stream(false);
        self.flush_stream(true);
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
                video: false,
            });
            ring
        };
        // Said before anything can come back: our own voice connection may be up before the
        // ring is even placed, and until someone answers that is not a call.
        if ring {
            self.ctx.ringing_out(conversation_id);
        }
        self.tell(4, voice_state(guild.as_deref(), Some(conversation_id), false, false, false));
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
        let (was, share_key, ringing) = {
            let mut state = self.state.lock().unwrap();
            let ours = state.voice.call.as_ref().is_some_and(|call| call.conversation == conversation_id);
            let was = if ours { state.voice.call.take() } else { None };
            let share_key = ours.then(|| state.voice.share.take()).flatten().map(|slot| slot.key).filter(|key| !key.is_empty());
            if ours {
                state.voice.watch.take();
            }
            (was, share_key, state.voice.ringing.remove(conversation_id))
        };
        match was {
            Some(call) => {
                self.tell(4, voice_state(call.guild.as_deref(), None, false, false, false));
                if let Some(key) = share_key {
                    self.tell(19, json!({ "stream_key": key }));
                }
            }
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
            voice_state(call.guild.as_deref(), Some(&call.conversation), muted, deafened, call.video)
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
                self.tell(4, voice_state(guild.as_deref(), None, false, false, false));
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
            self.tell(4, voice_state(call.guild.as_deref(), None, false, false, false));
            self.ctx.call_failed(&call.conversation, CallFailure::Lost, NO_SESSION);
        }
        false
    }

    /// The user asked for a camera, a screen, or to watch someone else's share.
    pub(super) fn call_picture(&self, picture: Picture) {
        match picture {
            Picture::Camera(on) => {
                let frame = {
                    let mut state = self.state.lock().unwrap();
                    let Some(call) = state.voice.call.as_mut() else {
                        return;
                    };
                    call.video = on;
                    voice_state(call.guild.as_deref(), Some(&call.conversation), call.muted, call.deafened, on)
                };
                self.tell(4, frame);
            }
            Picture::Share(true) => {
                let (guild, channel) = {
                    let mut state = self.state.lock().unwrap();
                    let Some(call) = state.voice.call.as_ref() else {
                        return;
                    };
                    if state.voice.share.as_ref().is_some_and(|slot| !slot.drop) {
                        return;
                    }
                    let guild = call.guild.clone();
                    let channel = call.conversation.clone();
                    state.voice.share = Some(StreamSlot {
                        key: String::new(),
                        rtc_server: None,
                        rtc_channel: None,
                        token: None,
                        endpoint: None,
                        told: false,
                        drop: false,
                    });
                    (guild, channel)
                };
                self.tell(18, json!({
                    "type": if guild.is_some() { "guild" } else { "call" },
                    "guild_id": guild,
                    "channel_id": channel,
                    "preferred_region": Value::Null,
                }));
            }
            Picture::Share(false) => {
                let _gate = self.stream_gate.lock().unwrap();
                let (key, conversation) = {
                    let mut state = self.state.lock().unwrap();
                    let conversation = state.voice.call.as_ref().map(|call| call.conversation.clone());
                    let key = match state.voice.share.as_mut() {
                        Some(slot) if slot.key.is_empty() => {
                            slot.drop = true;
                            None
                        }
                        Some(_) => state.voice.share.take().map(|slot| slot.key).filter(|key| !key.is_empty()),
                        None => None,
                    };
                    (key, conversation)
                };
                if let Some(key) = key {
                    self.tell(19, json!({ "stream_key": key }));
                }
                if let Some(conversation) = conversation {
                    self.ctx.tell_media(&conversation, MediaRequest::StreamStop { watch: false });
                }
            }
            Picture::Watch(Some(user)) => {
                let key = {
                    let mut state = self.state.lock().unwrap();
                    let Some(call) = state.voice.call.as_ref() else {
                        return;
                    };
                    let key = stream_key(call.guild.as_deref(), &call.conversation, &user);
                    state.voice.watch = Some(StreamSlot {
                        key: key.clone(),
                        rtc_server: None,
                        rtc_channel: None,
                        token: None,
                        endpoint: None,
                        told: false,
                        drop: false,
                    });
                    key
                };
                self.tell(20, json!({ "stream_key": key }));
            }
            Picture::Watch(None) => {
                let _gate = self.stream_gate.lock().unwrap();
                let conversation = {
                    let mut state = self.state.lock().unwrap();
                    state.voice.watch.take();
                    state.voice.call.as_ref().map(|call| call.conversation.clone())
                };
                if let Some(conversation) = conversation {
                    self.ctx.tell_media(&conversation, MediaRequest::StreamStop { watch: true });
                }
            }
        }
    }

    pub(super) fn stream_created(&self, data: &str) -> Result<(), Failure> {
        let made: StreamMade = serde_json::from_str(data)?;
        let next = {
            let mut state = self.state.lock().unwrap();
            let me = state.me.clone();
            let mine = owned_by(&made.stream_key, &me);
            if state.voice.call.is_none() {
                if mine { StreamNext::Delete(made.stream_key) } else { StreamNext::Quiet }
            } else if mine {
                let dropping = state.voice.share.as_ref().is_some_and(|slot| slot.drop);
                let fits = state.voice.share.as_ref().is_some_and(|slot| !slot.drop && (slot.key.is_empty() || slot.key == made.stream_key));
                if dropping {
                    state.voice.share = None;
                    StreamNext::Delete(made.stream_key)
                } else if fits {
                    let slot = state.voice.share.as_mut().expect("the share was just found");
                    note_stream(slot, &made);
                    StreamNext::Flush(false)
                } else {
                    StreamNext::Quiet
                }
            } else if state.voice.watch.as_ref().is_some_and(|slot| slot.key == made.stream_key) {
                let slot = state.voice.watch.as_mut().expect("the watch was just found");
                note_stream(slot, &made);
                StreamNext::Flush(true)
            } else {
                StreamNext::Quiet
            }
        };
        self.follow(next);
        Ok(())
    }

    pub(super) fn stream_server(&self, data: &str) -> Result<(), Failure> {
        let update: StreamServer = serde_json::from_str(data)?;
        let next = {
            let mut state = self.state.lock().unwrap();
            let me = state.me.clone();
            let watch = if state.voice.share.as_ref().is_some_and(|slot| slot.key == update.stream_key || (slot.key.is_empty() && owned_by(&update.stream_key, &me))) {
                Some(false)
            } else if state.voice.watch.as_ref().is_some_and(|slot| slot.key == update.stream_key) {
                Some(true)
            } else {
                None
            };
            let Some(watch) = watch else {
                return Ok(());
            };
            let slot = if watch { state.voice.watch.as_mut() } else { state.voice.share.as_mut() }.expect("the slot was just found");
            if slot.key.is_empty() {
                slot.key = update.stream_key;
            }
            slot.token = Some(update.token);
            match update.endpoint {
                Some(endpoint) => {
                    if slot.endpoint.as_ref() != Some(&endpoint) {
                        slot.told = false;
                    }
                    slot.endpoint = Some(endpoint);
                    StreamNext::Flush(watch)
                }
                None => {
                    slot.endpoint = None;
                    slot.told = false;
                    StreamNext::Pause(watch)
                }
            }
        };
        self.follow(next);
        Ok(())
    }

    pub(super) fn stream_deleted(&self, data: &str) -> Result<(), Failure> {
        let gone: StreamGone = serde_json::from_str(data)?;
        if !gone.reason.is_empty() {
            tracing::warn!("chatd: discord ended a stream ({})", gone.reason);
        }
        let next = {
            let mut state = self.state.lock().unwrap();
            let me = state.me.clone();
            if state.voice.share.as_ref().is_some_and(|slot| slot.key == gone.stream_key || (slot.key.is_empty() && owned_by(&gone.stream_key, &me))) {
                state.voice.share = None;
                StreamNext::Stop(false)
            } else if state.voice.watch.as_ref().is_some_and(|slot| slot.key == gone.stream_key) {
                state.voice.watch = None;
                StreamNext::Stop(true)
            } else {
                StreamNext::Quiet
            }
        };
        self.follow(next);
        Ok(())
    }

    fn follow(&self, next: StreamNext) {
        match next {
            StreamNext::Quiet => {}
            StreamNext::Delete(key) => self.tell(19, json!({ "stream_key": key })),
            StreamNext::Flush(watch) => self.flush_stream(watch),
            StreamNext::Pause(watch) => self.stop_leg(watch, false),
            StreamNext::Stop(watch) => self.stop_leg(watch, true),
        }
    }

    /// Gives mediad the stream session once Discord has named the server, the channel, the
    /// token and the endpoint, and the voice call has a session id. The MLS group is the RTC
    /// server id minus one.
    fn flush_stream(&self, watch: bool) {
        let _gate = self.stream_gate.lock().unwrap();
        let ready = {
            let mut state = self.state.lock().unwrap();
            let me = state.me.clone();
            let Some(call) = state.voice.call.as_ref() else {
                return;
            };
            let Some(session) = call.session.clone() else {
                return;
            };
            let conversation = call.conversation.clone();
            let (muted, deafened) = (call.muted, call.deafened);
            let slot = if watch { state.voice.watch.as_mut() } else { state.voice.share.as_mut() };
            let Some(slot) = slot else {
                return;
            };
            if slot.drop || slot.told {
                return;
            }
            let Some(rtc_server) = slot.rtc_server.clone() else {
                return;
            };
            let rtc_channel = slot.rtc_channel.clone().unwrap_or_else(|| conversation.clone());
            let Some(token) = slot.token.clone() else {
                return;
            };
            let Some(endpoint) = slot.endpoint.clone() else {
                return;
            };
            let Ok(server_num) = rtc_server.parse::<u64>() else {
                tracing::warn!("chatd: discord stream server id {rtc_server} is not a number");
                return;
            };
            slot.told = true;
            Some((
                conversation,
                VoiceJoin {
                    network: Network::Discord,
                    endpoint,
                    token,
                    session_id: session,
                    user_id: me,
                    server_id: rtc_server,
                    channel_id: rtc_channel,
                    dave: Dave { max_version: 1, required: true },
                    muted,
                    deafened,
                    stream: true,
                    watch,
                    group_id: server_num.saturating_sub(1).to_string(),
                },
            ))
        };
        if let Some((conversation, join)) = ready {
            self.ctx.tell_media(&conversation, MediaRequest::Stream(join));
        }
    }

    /// Stops the mediad leg. `forget` also turns the button off: the stream is gone, not merely
    /// between servers.
    fn stop_leg(&self, watch: bool, forget: bool) {
        let _gate = self.stream_gate.lock().unwrap();
        let conversation = {
            let state = self.state.lock().unwrap();
            state.voice.call.as_ref().map(|call| call.conversation.clone())
        };
        let Some(conversation) = conversation else {
            return;
        };
        self.ctx.tell_media(&conversation, MediaRequest::StreamStop { watch });
        if forget {
            let request = if watch {
                MediaRequest::ShareWatch { source: None }
            } else {
                MediaRequest::ShareSend { on: false, screen: None, quality: None }
            };
            self.ctx.tell_media(&conversation, request);
        }
    }
}

enum StreamNext {
    Quiet,
    Flush(bool),
    /// The server went away. Disconnect and wait for the next endpoint.
    Pause(bool),
    /// The stream ended. Disconnect, and turn the control off.
    Stop(bool),
    Delete(String),
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Snowflake {
    Text(String),
    Number(u64),
}

fn snowflake(id: Option<Snowflake>) -> Option<String> {
    id.map(|id| match id {
        Snowflake::Text(text) => text,
        Snowflake::Number(number) => number.to_string(),
    })
}

#[derive(Deserialize)]
struct StreamMade {
    stream_key: String,
    #[serde(default)]
    rtc_server_id: Option<Snowflake>,
    #[serde(default)]
    rtc_channel_id: Option<Snowflake>,
}

fn note_stream(slot: &mut StreamSlot, made: &StreamMade) {
    if slot.key != made.stream_key {
        slot.key = made.stream_key.clone();
    }
    if let Some(server) = snowflake(made.rtc_server_id.clone()) {
        if slot.rtc_server.as_ref() != Some(&server) {
            slot.told = false;
        }
        slot.rtc_server = Some(server);
    }
    if let Some(channel) = snowflake(made.rtc_channel_id.clone()) {
        slot.rtc_channel = Some(channel);
    }
}

#[derive(Deserialize)]
struct StreamServer {
    token: String,
    stream_key: String,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Deserialize)]
struct StreamGone {
    stream_key: String,
    #[serde(default)]
    reason: String,
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
            video: false,
        });
        assert!(voice.watched("901", Some("82")), "the call we are in, wherever it is");
    }

    #[test]
    fn a_guild_stream_is_named_by_the_guild_and_a_direct_call_is_not() {
        assert_eq!(stream_key(Some("81"), "900", "111"), "guild:81:900:111");
        assert_eq!(stream_key(None, "3001", "111"), "call:3001:111");
        assert!(owned_by("guild:81:900:111", "111"));
        assert!(!owned_by("call:3001:222", "111"));
    }
}
