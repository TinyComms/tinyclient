//! Call signalling. The media is mediad's: this joins and leaves on the gateway, hands over the
//! voice session Discord gives out, and reports who is in which voice channel, who is in the
//! call, who is ringing, and when the other side picked up.

use super::super::{Failure, Picture, Refusal};
use super::model::{self, VoiceState};
use super::rest::Refused;
use super::Discord;
use protocol::{
    CallFailure, CallSupport, ConversationKind, Dave, MediaFile, MediaRequest, Network, Occupant, Participant, Relation, VoiceJoin, VoiceQuality, Watching,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

mod settings;

/// Voice states remembered across every server. A person in a voice channel is one entry.
const HEARD: usize = 2_000;

#[derive(Deserialize)]
pub(super) struct ChannelStatus {
    id: String,
    status: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ChannelStatuses {
    guild_id: String,
    channels: Vec<ChannelStatus>,
}

impl Discord {
    pub(super) fn voice_channel_status(&self, update: ChannelStatus) {
        let status = update.status.map(|text| {
            let text: String = text.lines().next().unwrap_or_default().chars().take(500).collect();
            let files: Vec<_> = text.split_inclusive('>').filter_map(|fragment| {
                let start = fragment.rfind('<')?;
                let (_, id, animated) = protocol::discord_emoji_token(&fragment[start..])?;
                Some(model::emoji(id, animated))
            }).collect();
            let _ = self.ctx.media(&files);
            text
        });
        self.ctx.voice_status(&update.id, status);
    }

    pub(super) fn voice_channel_statuses(&self, update: ChannelStatuses) {
        let current: HashSet<_> = update.channels.iter().map(|status| status.id.as_str()).collect();
        for key in self.ctx.voice_status_channels(&update.guild_id) {
            if !current.contains(key.id.as_str()) {
                self.ctx.voice_status(&key.id, None);
            }
        }
        for status in update.channels {
            self.voice_channel_status(status);
        }
    }

    pub(super) fn set_voice_status(&self, channel: &str, status: Option<String>) -> Result<(), Refusal> {
        let row = self.ctx.known(channel).ok().flatten().filter(|row| row.kind == ConversationKind::Voice)
            .ok_or_else(|| Refusal::Refused("This voice channel is no longer available.".into()))?;
        let joined = {
            let state = self.state.lock().unwrap();
            state.voice.states.get(&state.me).is_some_and(|heard| heard.channel == channel)
        };
        if !row.rights.set_voice_status || (!joined && !row.rights.rename) {
            return Err(Refusal::Refused("You cannot set this voice channel's status.".into()));
        }
        let status = status.map(|text| text.trim().chars().take(500).collect::<String>()).filter(|text| !text.is_empty());
        self.call("PUT", &format!("/channels/{channel}/voice-status"), Some(json!({"status": status})))
            .map(|_| ()).map_err(super::refusal)
    }

    /// The current channel and guild are checked against the latest gateway state.
    fn voice_target(&self, channel: &str, person: &str) -> Result<String, Refusal> {
        let state = self.state.lock().unwrap();
        state.voice.states.get(person).filter(|heard| heard.channel == channel && person != state.me)
            .and_then(|heard| heard.guild.clone())
            .ok_or_else(|| Refusal::Refused("That participant is no longer in this server voice channel.".into()))
    }

    pub(super) fn moderate_voice(&self, channel: &str, person: &str, mute: Option<bool>, deaf: Option<bool>) -> Result<(), Refusal> {
        let guild = self.voice_target(channel, person)?;
        let mut body = serde_json::Map::new();
        if let Some(mute) = mute {
            self.require_channel_perm(&guild, channel, model::MUTE_MEMBERS, "server mute this participant")?;
            body.insert("mute".into(), json!(mute));
        }
        if let Some(deaf) = deaf {
            self.require_channel_perm(&guild, channel, model::DEAFEN_MEMBERS, "server deafen this participant")?;
            body.insert("deaf".into(), json!(deaf));
        }
        if body.is_empty() {
            return Ok(());
        }
        self.call("PATCH", &format!("/guilds/{guild}/members/{person}"), Some(Value::Object(body)))
            .map(|_| ()).map_err(super::refusal)
    }

    pub(super) fn move_in_voice(&self, channel: &str, person: &str, to: Option<&str>) -> Result<(), Refusal> {
        let guild = self.voice_target(channel, person)?;
        self.require_channel_perm(&guild, channel, model::MOVE_MEMBERS, "move or disconnect this participant")?;
        if let Some(to) = to {
            let valid = self.ctx.known(to).ok().flatten().is_some_and(|row| {
                row.kind == ConversationKind::Voice && row.space.as_deref() == Some(guild.as_str())
                    && self.channel_permission_bits(&guild, to).is_some_and(|bits| bits & model::CONNECT != 0)
            });
            if !valid {
                return Err(Refusal::Refused("Choose a voice channel you can join on this server.".into()));
            }
        }
        self.call("PATCH", &format!("/guilds/{guild}/members/{person}"), Some(json!({"channel_id": to})))
            .map(|_| ()).map_err(super::refusal)
    }
}
/// How long Discord has to hand out a voice session.
const PATIENCE: Duration = Duration::from_secs(15);
/// How long our own voice state has to show up in a direct call before the other side is rung.
const RING_AFTER: Duration = Duration::from_secs(5);
/// A 1:1 ring that disappears sooner than this (without us stopping it) was declined; after this
/// Discord timed out the ring itself (rang out).
const RING_DECLINE_WINDOW: Duration = Duration::from_secs(50);
/// The flags the web client sends with its voice state. From the community documentation;
/// unconfirmed against a real account.
const FLAGS: u8 = 2;

/// A Discord group DM holds at most ten people, including us.
const GROUP_DM_LIMIT: usize = 10;

/// The words of each way a call ends here. No status code or Discord error text is among them.
const NO_ANSWER: &str = "Nobody picked up.";
/// The dock tells a decline from no answer by this sentence.
// CONTRACT: `CallFailure` has one reason, `NoAnswer`, for "declined" and "rang out".
const DECLINED: &str = "They declined the call.";
const REMOVED: &str = "Discord took you out of the call. You were removed, or the channel is gone.";
const NO_SESSION: &str = "No reply from Discord after 15 seconds.";

/// What pressing the call button of a server channel does. A voice channel is joined here. A
/// stage is not: a listener there cannot simply speak, so it opens in Discord.
pub(super) fn support(kind: u8) -> CallSupport {
    match kind {
        model::VOICE => CallSupport::Native,
        model::STAGE => CallSupport::Handoff,
        _ => CallSupport::None,
    }
}

/// Discord's own default, in kb/s. A direct or group call stays here.
const DIRECT_KBPS: u32 = 64;
/// The quietest voice channel Discord publishes, and libopus stereo's ceiling.
const CHANNEL_KBPS: std::ops::RangeInclusive<u32> = 8..=510;

/// Encode target in kb/s. `channel_bps` is the voice channel's bitrate in bits per second,
/// 0 when it did not say. A direct call ignores it and stays at 64 (32 on data saver).
/// A channel is sent at the rate Discord set: a quality preset caps that rate and never
/// raises it. Not [`VoiceQuality::apply_room`], which clamps a Tiny room to 16–128.
pub(super) fn voice_kbps(channel_bps: u32, direct: bool, quality: VoiceQuality) -> u32 {
    if direct {
        return match quality {
            VoiceQuality::DataSaver => DIRECT_KBPS.min(32),
            VoiceQuality::Standard | VoiceQuality::High => DIRECT_KBPS,
        };
    }
    let kbps = if channel_bps == 0 { DIRECT_KBPS } else { channel_bps / 1_000 };
    let kbps = kbps.clamp(*CHANNEL_KBPS.start(), *CHANNEL_KBPS.end());
    match quality {
        VoiceQuality::DataSaver => kbps.min(32),
        VoiceQuality::Standard | VoiceQuality::High => kbps,
    }
}

/// Where our own voice state puts the call we are in.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Follow {
    /// Still the channel we joined.
    Here,
    /// Same guild, another channel. The official client follows a move, including one from another device.
    Moved(String),
    /// Another guild, or a direct call: not this call.
    Elsewhere,
    /// Discord took us out of the channel.
    Left,
}

/// `guild` and `channel` are the voice state. `call_guild` is set for a server channel and
/// absent for a direct or group call, which is left where it is.
pub(super) fn follow(guild: Option<&str>, channel: Option<&str>, call_guild: Option<&str>, call_channel: &str) -> Follow {
    match channel {
        Some(channel) if channel == call_channel => Follow::Here,
        Some(channel) if call_guild.is_some() && guild == call_guild => Follow::Moved(channel.to_owned()),
        Some(_) => Follow::Elsewhere,
        None => Follow::Left,
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
    avatar: Option<MediaFile>,
    muted: bool,
    deafened: bool,
    server_muted: bool,
    server_deafened: bool,
    suppressed: bool,
    camera: bool,
    sharing: bool,
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
    /// When the current ring began, to tell an early decline from a ring that timed out.
    ring_started: Option<Instant>,
    /// True when we explicitly called `stop-ringing`, so a disappearing ring is not a decline.
    stopped_by_us: bool,
    /// chatd has been told the other side picked up.
    answered: bool,
    /// Our camera is on. Discord is told with the voice state, and mediad tells it again on the
    /// voice gateway once a video SSRC exists.
    video: bool,
    /// The kb/s last handed to mediad, so the same rate is not sent again.
    told_kbps: Option<u32>,
    /// The next report replaces a session mediad already has, so it is a rejoin.
    again: bool,
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
    call_regions: HashMap<String, String>,
    regions: Option<(Instant, Vec<protocol::DiscordVoiceRegion>)>,
    /// Direct conversations whose call is ringing us.
    ringing: HashSet<String>,
    /// User ids currently being rung per direct/group call, from `CALL_CREATE` / `CALL_UPDATE`.
    ringing_out: HashMap<String, Vec<String>>,
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
        self.ringing_out.clear();
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
    region: Option<String>,
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
            let mut participants: Vec<Participant> = there
                .map(|(id, heard)| Participant {
                    id: id.clone(),
                    name: heard.name.clone(),
                    avatar: heard.avatar.clone(),
                    speaking: false,
                    muted: heard.muted,
                    deafened: heard.deafened,
                    server_muted: heard.server_muted,
                    server_deafened: heard.server_deafened,
                    suppressed: heard.suppressed,
                    camera: heard.camera,
                    sharing: heard.sharing,
                    ringing: false,
                    ..Participant::default()
                })
                .collect();
            let joined_any = !participants.is_empty();
            if let Some(rung) = state.voice.ringing_out.get(&call.conversation) {
                for id in rung {
                    if *id == state.me || participants.iter().any(|p| p.id == *id) {
                        continue;
                    }
                    let name = state.users.get(id).cloned().unwrap_or_else(|| id.clone());
                    participants.push(Participant {
                        id: id.clone(),
                        name,
                        ringing: true,
                        ..Participant::default()
                    });
                }
            }
            let answered = call.ring && !call.answered && joined_any;
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
                    avatar: heard.avatar.clone(),
                    muted: heard.muted,
                    deafened: heard.deafened,
                    server_muted: heard.server_muted,
                    server_deafened: heard.server_deafened,
                    suppressed: heard.suppressed,
                    streaming: heard.sharing,
                    camera: heard.camera,
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
        let emptied;
        let mut media_files: Vec<(MediaFile, String)> = Vec::new();
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
                let voice_guild = voice.guild_id.clone().or_else(|| guild.map(str::to_owned));
                let named = voice.member.as_ref().map(|member| member.name().to_owned()).filter(|name| !name.is_empty());
                let named = named.or_else(|| state.users.get(&voice.user_id).cloned());
                let me = (voice.user_id == state.me).then_some("You".to_owned());
                let avatar = voice
                    .member
                    .as_ref()
                    .and_then(|member| member.avatar(voice_guild.as_deref()))
                    .map(|(file, url)| {
                        media_files.push((file.clone(), url));
                        file
                    })
                    .or_else(|| before.as_ref().and_then(|before| before.avatar.clone()));
                let is_stage = stages.contains(channel.as_str());
                let heard = Heard {
                    guild: voice_guild,
                    channel: channel.clone(),
                    name: named.or(before.map(|before| before.name)).or(me).unwrap_or_default(),
                    avatar,
                    muted: voice.self_mute,
                    deafened: voice.self_deaf,
                    server_muted: voice.mute,
                    server_deafened: voice.deaf,
                    suppressed: !is_stage && voice.suppress,
                    camera: voice.self_video,
                    sharing: voice.self_stream,
                    live: is_stage && !voice.suppress,
                };
                changed.insert(channel.clone());
                state.voice.states.insert(voice.user_id.clone(), heard);
            }
            emptied = changed.iter().filter(|channel| !state.voice.states.values().any(|heard| &heard.channel == *channel))
                .cloned().collect::<Vec<_>>();
        }
        for channel in emptied {
            self.ctx.voice_status(&channel, None);
        }
        if !media_files.is_empty() {
            let _ = self.ctx.media(&media_files);
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
            let (session, endpoint, token) = (session.clone(), endpoint.clone(), token.clone());
            let again = call.again;
            call.reported = true;
            call.again = false;
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
                stream_owner: None,
                group_id: String::new(),
            };
            (call.conversation.clone(), join, again)
        };
        let channel = join.1.channel_id.clone();
        if join.2 {
            tracing::info!("discord voice rejoin channel={channel}");
        } else {
            tracing::info!("discord voice session channel={channel}");
        }
        self.ctx.voice(&join.0, join.1);
        self.tell_bitrate();
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
        enum Ours {
            Left(String),
            Moved { from: String, to: String },
            Here(String),
            Same,
        }
        let ours = {
            let mut state = self.state.lock().unwrap();
            let Some(call) = &mut state.voice.call else {
                return;
            };
            let guild = voice.guild_id.as_deref();
            let channel = voice.channel_id.as_deref();
            match follow(guild, channel, call.guild.as_deref(), &call.conversation) {
                Follow::Here => {
                    if call.session != voice.session_id {
                        call.session = voice.session_id.clone();
                        call.reported = false;
                    }
                    Ours::Here(call.conversation.clone())
                }
                Follow::Moved(to) => {
                    let from = std::mem::replace(&mut call.conversation, to.clone());
                    // mediad already has this session when both halves arrived, so the next
                    // report is a rejoin carrying the channel we were moved to.
                    call.again = call.session.is_some() && call.server.is_some();
                    if call.session.as_ref() != voice.session_id.as_ref() {
                        call.session = voice.session_id.clone();
                    }
                    call.reported = false;
                    Ours::Moved { from, to }
                }
                Follow::Elsewhere => Ours::Same,
                // We were in and Discord took us out: kicked, or the channel is gone.
                Follow::Left if call.reported => match state.voice.call.take() {
                    Some(call) => Ours::Left(call.conversation),
                    None => Ours::Same,
                },
                Follow::Left => Ours::Same,
            }
        };
        match ours {
            Ours::Left(conversation) => self.ctx.call_failed(&conversation, CallFailure::Refused, REMOVED),
            Ours::Moved { from, to } => {
                self.ctx.tiny_moved(&from, &to);
                self.ctx.self_moderation(&to, voice.mute, voice.deaf, voice.suppress);
                self.roster();
                self.occupancy(BTreeSet::from([from, to]));
                self.report();
            }
            Ours::Here(conversation) => {
                self.ctx.self_moderation(&conversation, voice.mute, voice.deaf, voice.suppress);
                self.report();
            }
            Ours::Same => self.report(),
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
        let mut can_speak = true;
        if let Some(guild_id) = guild.as_deref() {
            let kept_channel = self
                .ctx
                .note(&format!("kept:{guild_id}"))
                .and_then(|kept| serde_json::from_str::<Vec<model::Kept>>(&kept).ok())
                .and_then(|kept| kept.into_iter().find(|ch| ch.id == conversation_id))
                .map(model::Kept::channel);
            let me = self.me();
            let clock = crate::hub::now();
            let state = self.state.lock().unwrap();
            let mut can_move = false;
            if let Some(info) = state.guilds.get(guild_id) {
                if let Some(until) = info.timed_out(clock) {
                    return refuse(&super::ready::timeout_reason(until, clock));
                }
                if let Some(ch) = kept_channel.as_ref() {
                    let perms = info.permissions(guild_id, &me, ch);
                    if perms & model::CONNECT == 0 {
                        return refuse("You don't have permission to connect to this voice channel.");
                    }
                    can_move = perms & model::MOVE_MEMBERS != 0;
                    can_speak = perms & model::SPEAK != 0;
                }
            }
            if conversation.limit > 0 && !can_move {
                let others = state
                    .voice
                    .states
                    .iter()
                    .filter(|(user, heard)| heard.channel == conversation_id && user.as_str() != state.me)
                    .count();
                if others >= conversation.limit as usize {
                    return refuse("This channel is full.");
                }
            }
        }
        let start_muted = !can_speak;
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
                muted: start_muted,
                deafened: false,
                ring,
                saw_ringing: false,
                ring_started: ring.then(Instant::now),
                stopped_by_us: false,
                answered: false,
                video: false,
                told_kbps: None,
                again: false,
            });
            ring
        };
        // Said before anything can come back: our own voice connection may be up before the
        // ring is even placed, and until someone answers that is not a call.
        if ring {
            self.ctx.ringing_out(conversation_id);
        }
        self.tell(4, voice_state(guild.as_deref(), Some(conversation_id), start_muted, false, false));
        // Whoever is already there, before anyone's state changes.
        self.roster();
        self.occupancy(BTreeSet::from([conversation_id.to_owned()]));
        self.tell_bitrate();
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

    /// Rings everyone (`None`) or specific people (`Some`) in an active direct or group DM call.
    /// Single explicit click -> single POST request, never retried.
    pub(super) fn call_ring(&self, conversation_id: &str, people: Option<Vec<String>>) -> Result<(), super::Refusal> {
        {
            let mut state = self.state.lock().unwrap();
            let Some(call) = state.voice.call.as_mut().filter(|call| call.conversation == conversation_id) else {
                return Err(super::Refusal::Refused("You are not in this call.".to_owned()));
            };
            if call.guild.is_some() {
                return Err(super::Refusal::Unsupported);
            }
            call.stopped_by_us = false;
            call.ring_started = Some(Instant::now());
        }
        let body = match people {
            Some(ids) => json!({ "recipients": ids }),
            None => json!({ "recipients": null }),
        };
        self.call("POST", &format!("/channels/{conversation_id}/call/ring"), Some(body))
            .map(drop)
            .map_err(|refused| super::Refusal::Refused(ring_refused(&refused)))
    }

    /// Stops ringing everyone (`None`) or specific people (`Some`) in an active direct or group DM call.
    pub(super) fn call_stop_ringing(&self, conversation_id: &str, people: Option<Vec<String>>) -> Result<(), super::Refusal> {
        {
            let mut state = self.state.lock().unwrap();
            let Some(call) = state.voice.call.as_mut().filter(|call| call.conversation == conversation_id) else {
                return Err(super::Refusal::Refused("You are not in this call.".to_owned()));
            };
            if call.guild.is_some() {
                return Err(super::Refusal::Unsupported);
            }
            call.stopped_by_us = true;
        }
        let body = match people {
            Some(ids) => json!({ "recipients": ids }),
            None => json!({}),
        };
        self.call("POST", &format!("/channels/{conversation_id}/call/stop-ringing"), Some(body))
            .map(drop)
            .map_err(super::refusal)
    }

    /// Adds friends to an active DM or group DM call.
    ///
    /// - In a group DM: `PUT /channels/{group}/recipients/{user}`, then rings just them.
    /// - In a 1:1 DM: creates a group DM (`POST /users/@me/channels` with `[peer, ...added]`),
    ///   joins its voice state and rings everyone, moving the call there; if ringing fails after
    ///   the group was created, restores the 1:1 voice state and leaves the call where it was.
    pub(super) fn call_add_people(&self, conversation_id: &str, people: Vec<String>) -> Result<String, super::Refusal> {
        let Ok(Some(conversation)) = self.ctx.known(conversation_id) else {
            return Err(super::Refusal::Refused("This conversation is gone.".to_owned()));
        };
        if !conversation.kind.is_direct() {
            return Err(super::Refusal::Unsupported);
        }
        let (me, added, peer_for_direct, current_group_count, audio_state) = {
            let state = self.state.lock().unwrap();
            let Some(call) = state.voice.call.as_ref().filter(|call| call.conversation == conversation_id) else {
                return Err(super::Refusal::Refused("You are not in this call.".to_owned()));
            };
            if call.guild.is_some() {
                return Err(super::Refusal::Unsupported);
            }
            let mut added: Vec<String> = Vec::new();
            for id in people {
                if id.is_empty() || id == state.me || added.contains(&id) {
                    continue;
                }
                if state.relations.get(&id) != Some(&Relation::Friend) {
                    return Err(super::Refusal::Refused("Only friends can be added to a Discord call.".to_owned()));
                }
                added.push(id);
            }
            if added.is_empty() {
                return Err(super::Refusal::Refused("Choose at least one friend to add.".to_owned()));
            }
            let peer = state
                .directs
                .iter()
                .find(|(_, ch)| ch.as_str() == conversation_id)
                .map(|(user, _)| user.clone())
                .or_else(|| {
                    state
                        .voice
                        .states
                        .iter()
                        .find(|(id, h)| h.channel == conversation_id && **id != state.me)
                        .map(|(id, _)| id.clone())
                });
            let group_members = state
                .groups
                .get(conversation_id)
                .map(|g| g.recipients.len() + 1)
                .unwrap_or(2);
            (state.me.clone(), added, peer, group_members, (call.muted, call.deafened, call.video))
        };

        if conversation.kind == ConversationKind::Group {
            let existing: HashSet<String> = {
                let state = self.state.lock().unwrap();
                state
                    .groups
                    .get(conversation_id)
                    .map(|g| g.recipients.iter().cloned().collect())
                    .unwrap_or_default()
            };
            let to_add: Vec<String> = added.into_iter().filter(|id| !existing.contains(id)).collect();
            if to_add.is_empty() {
                return Ok(conversation_id.to_owned());
            }
            if current_group_count + to_add.len() > GROUP_DM_LIMIT {
                return Err(super::Refusal::Refused(format!(
                    "A Discord group call can hold at most {GROUP_DM_LIMIT} people."
                )));
            }
            for user in &to_add {
                self.call("PUT", &format!("/channels/{conversation_id}/recipients/{user}"), None)
                    .map_err(super::refusal)?;
                if let Some(group) = self.state.lock().unwrap().groups.get_mut(conversation_id) {
                    if !group.recipients.contains(user) {
                        group.recipients.push(user.clone());
                    }
                }
                self.call("POST", &format!("/channels/{conversation_id}/call/ring"), Some(json!({ "recipients": [user] })))
                    .map_err(|refused| super::Refusal::Refused(ring_refused(&refused)))?;
            }
            return Ok(conversation_id.to_owned());
        }

        // 1:1 DM -> escalate to a new group DM, join its call, ring everyone, and move the call.
        let mut recipients: Vec<String> = Vec::new();
        if let Some(peer) = peer_for_direct {
            if peer != me {
                recipients.push(peer);
            }
        }
        for id in added {
            if !recipients.contains(&id) {
                recipients.push(id);
            }
        }
        if recipients.len() + 1 > GROUP_DM_LIMIT {
            return Err(super::Refusal::Refused(format!(
                "A Discord group call can hold at most {GROUP_DM_LIMIT} people."
            )));
        }

        // Step 1: create the group DM. If this fails (including Captcha -> Refused::Challenge),
        // the 1:1 call is untouched.
        let created = self
            .call("POST", "/users/@me/channels", Some(json!({ "recipients": recipients })))
            .map_err(super::refusal)?;
        let channel: model::Channel = serde_json::from_str(&created)
            .map_err(|err| super::Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let new_id = channel.id.clone();
        self.direct(&channel, &HashMap::new())
            .map_err(|err| super::Refusal::Refused(err.to_string()))?;

        // Step 2: move our voice state to the new group channel.
        let (muted, deafened, video) = audio_state;
        {
            let mut state = self.state.lock().unwrap();
            let Some(call) = state.voice.call.as_mut().filter(|call| call.conversation == conversation_id) else {
                return Err(super::Refusal::Refused("The call ended while adding people.".to_owned()));
            };
            call.conversation = new_id.clone();
            call.session = None;
            call.server = None;
            call.reported = false;
            call.again = true;
            call.ring = true;
            call.saw_ringing = false;
            call.ring_started = Some(Instant::now());
            call.stopped_by_us = false;
            call.answered = false;
        }
        // Tell chatd/shell the call moved BEFORE OP 4 so VOICE_STATE_UPDATE / VOICE_SERVER_UPDATE
        // arriving while the HTTP ring request is in flight are not dropped by `Calls::join`.
        self.ctx.tiny_moved(conversation_id, &new_id);
        self.tell(4, voice_state(None, Some(&new_id), muted, deafened, video));

        // Step 3: ring everyone in the new group DM. If ringing fails, undo Step 2 and return
        // our voice state to the original 1:1 call.
        if let Err(refused) = self.call("POST", &format!("/channels/{new_id}/call/ring"), Some(json!({ "recipients": null }))) {
            self.log("ring", &refused);
            {
                let mut state = self.state.lock().unwrap();
                if let Some(call) = state.voice.call.as_mut().filter(|call| call.conversation == new_id) {
                    call.conversation = conversation_id.to_owned();
                    call.session = None;
                    call.server = None;
                    call.reported = false;
                    call.again = true;
                    call.ring = false;
                }
            }
            self.ctx.tiny_moved(&new_id, conversation_id);
            self.tell(4, voice_state(None, Some(conversation_id), muted, deafened, video));
            return Err(super::Refusal::Refused(ring_refused(&refused)));
        }

        self.roster();
        self.occupancy(BTreeSet::from([conversation_id.to_owned(), new_id.clone()]));
        Ok(new_id)
    }

    /// A direct call began or its ringing changed.
    pub(super) fn call_changed(&self, event: CallEvent) {
        {
            let mut state = self.state.lock().unwrap();
            state.voice.calls.insert(event.channel_id.clone());
            if let Some(region) = event.region.as_ref().filter(|region| !region.is_empty() && region.len() <= 64) {
                if state.voice.call_regions.contains_key(&event.channel_id) || state.voice.call_regions.len() < HEARD {
                    state.voice.call_regions.insert(event.channel_id.clone(), region.clone());
                }
            }
            if event.ringing.is_empty() {
                state.voice.ringing_out.remove(&event.channel_id);
            } else {
                state.voice.ringing_out.insert(event.channel_id.clone(), event.ringing.clone());
            }
        }
        // Whoever joined is the answer to a ring we placed (see `roster`).
        self.voice_states(None, &event.voice_states);
        // Also refresh roster when `ringing` changed even if `voice_states` was empty.
        if event.voice_states.is_empty() {
            self.roster();
        }
        enum Now {
            Ring(String),
            Quiet,
            Unanswered { guild: Option<String>, declined: bool },
            Nothing,
        }
        let now = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let rings_me = event.ringing.contains(&state.me);
            let others = state.voice.states.iter().any(|(id, heard)| heard.channel == event.channel_id && *id != state.me);
            match state.voice.call.as_mut().filter(|call| call.conversation == event.channel_id) {
                Some(call) if call.ring => {
                    if !event.ringing.is_empty() {
                        call.saw_ringing = true;
                        call.ring_started.get_or_insert_with(Instant::now);
                    }
                    if call.saw_ringing && event.ringing.is_empty() && !others && !call.answered {
                        let early = call.ring_started.is_some_and(|at| at.elapsed() < RING_DECLINE_WINDOW);
                        let declined = early && !call.stopped_by_us;
                        let guild = state.voice.call.take().and_then(|call| call.guild);
                        Now::Unanswered { guild, declined }
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
            Now::Unanswered { guild, declined } => {
                self.tell(4, voice_state(guild.as_deref(), None, false, false, false));
                let one = self.ctx.known(&event.channel_id).ok().flatten().is_some_and(|row| row.kind == ConversationKind::Direct);
                let words = if one && declined { DECLINED } else { NO_ANSWER };
                self.ctx.call_failed(&event.channel_id, CallFailure::NoAnswer, words);
            }
            Now::Nothing => {}
        }
    }

    pub(super) fn call_deleted(&self, event: CallEvent) {
        {
            let mut state = self.state.lock().unwrap();
            state.voice.calls.remove(&event.channel_id);
            state.voice.call_regions.remove(&event.channel_id);
            state.voice.ringing.remove(&event.channel_id);
            state.voice.ringing_out.remove(&event.channel_id);
            state.voice.states.retain(|_, heard| heard.channel != event.channel_id);
        }
        self.ctx.ring_stopped(&event.channel_id);
        self.roster();
        self.occupancy(BTreeSet::from([event.channel_id]));
    }

    /// The voice channel's bitrate, in bits per second, from the note kept at connect.
    fn channel_bps(&self, guild: &str, channel: &str) -> u32 {
        let Some(note) = self.ctx.note(&format!("kept:{guild}")) else {
            return 0;
        };
        let kept: Vec<model::Kept> = serde_json::from_str(&note).unwrap_or_default();
        kept.into_iter().find(|row| row.id == channel).map(|row| row.bitrate).unwrap_or(0)
    }

    /// Asks mediad for this call's encode rate. The same rate is not sent twice. Nothing is
    /// sent until mediad is up, so a join that races the process is retried from `voice_tick`.
    fn tell_bitrate(&self) {
        let quality = self.ctx.hub.settings().voice_quality();
        let (conversation, guild, already) = {
            let state = self.state.lock().unwrap();
            let Some(call) = state.voice.call.as_ref() else {
                return;
            };
            (call.conversation.clone(), call.guild.clone(), call.told_kbps)
        };
        let direct = guild.is_none();
        let bps = guild.as_deref().map(|guild| self.channel_bps(guild, &conversation)).unwrap_or(0);
        let kbps = voice_kbps(bps, direct, quality);
        if already == Some(kbps) || !self.ctx.in_call(&conversation) {
            return;
        }
        self.ctx.tell_media(&conversation, MediaRequest::Bitrate { kbps });
        self.ctx.tell_media(&conversation, MediaRequest::Quality(quality));
        tracing::info!("discord voice bitrate kbps={kbps}");
        let mut state = self.state.lock().unwrap();
        if let Some(call) = state.voice.call.as_mut().filter(|call| call.conversation == conversation) {
            call.told_kbps = Some(kbps);
        }
    }

    /// Gives up on a join Discord never answered. True while one is waiting.
    pub(super) fn voice_tick(&self) -> bool {
        // A channel bitrate or a quality preset can change while the call is up. This does not
        // keep the socket waking: once the session is reported, the wait stays idle.
        if self.state.lock().unwrap().voice.call.is_some() {
            self.tell_bitrate();
        }
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
                let _gate = self.stream_gate.lock().unwrap();
                let (key, replaced) = {
                    let mut state = self.state.lock().unwrap();
                    let Some(call) = state.voice.call.as_ref() else {
                        return;
                    };
                    let key = stream_key(call.guild.as_deref(), &call.conversation, &user);
                    let replaced = state.voice.watch.as_ref()
                        .filter(|slot| slot.key != key)
                        .map(|_| call.conversation.clone());
                    state.voice.watch = Some(StreamSlot {
                        key: key.clone(),
                        rtc_server: None,
                        rtc_channel: None,
                        token: None,
                        endpoint: None,
                        told: false,
                        drop: false,
                    });
                    (key, replaced)
                };
                if let Some(conversation) = replaced {
                    self.ctx.tell_media(&conversation, MediaRequest::StreamStop { watch: true });
                }
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
                    stream_owner: slot.key.rsplit_once(':').filter(|_| watch).map(|(_, owner)| owner.to_owned()),
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
                MediaRequest::ShareSend { on: false, screen: None, quality: None, show_cursor: None }
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
            ring_started: None,
            stopped_by_us: false,
            answered: false,
            video: false,
            told_kbps: None,
            again: false,
        });
        assert!(voice.watched("901", Some("82")), "the call we are in, wherever it is");
    }

    #[test]
    fn the_channel_bitrate_is_what_is_sent_and_a_direct_call_stays_at_64() {
        use VoiceQuality::*;
        assert_eq!(voice_kbps(32_000, false, Standard), 32);
        assert_eq!(voice_kbps(8_000, false, Standard), 8);
        assert_eq!(voice_kbps(384_000, false, High), 384);
        assert_eq!(voice_kbps(32_000, false, High), 32, "high does not raise a channel");
        assert_eq!(voice_kbps(96_000, false, DataSaver), 32);
        assert_eq!(voice_kbps(8_000, false, DataSaver), 8, "data saver does not lift a quiet channel");
        assert_eq!(voice_kbps(0, false, Standard), 64, "a channel that did not say stays at 64");
        assert_eq!(voice_kbps(512_000, false, Standard), 510);
        assert_eq!(voice_kbps(32_000, true, Standard), 64);
        assert_eq!(voice_kbps(32_000, true, High), 64);
        assert_eq!(voice_kbps(0, true, DataSaver), 32);
    }

    #[test]
    fn a_same_guild_move_is_followed_and_a_direct_call_is_not() {
        assert_eq!(follow(Some("4000"), Some("4203"), Some("4000"), "4201"), Follow::Moved("4203".into()));
        assert_eq!(follow(Some("4000"), Some("4201"), Some("4000"), "4201"), Follow::Here);
        assert_eq!(follow(Some("9"), Some("1"), Some("4000"), "4201"), Follow::Elsewhere);
        assert_eq!(follow(None, Some("9"), None, "3001"), Follow::Elsewhere);
        assert_eq!(follow(Some("4000"), None, Some("4000"), "4201"), Follow::Left);
    }

    #[test]
    fn a_guild_stream_is_named_by_the_guild_and_a_direct_call_is_not() {
        assert_eq!(stream_key(Some("81"), "900", "111"), "guild:81:900:111");
        assert_eq!(stream_key(None, "3001", "111"), "call:3001:111");
        assert!(owned_by("guild:81:900:111", "111"));
        assert!(!owned_by("call:3001:222", "111"));
    }

    #[test]
    fn heard_keeps_camera_and_sharing_separate() {
        let cam_only = Heard {
            guild: Some("g".into()),
            channel: "c".into(),
            name: "Ada".into(),
            avatar: None,
            muted: false,
            deafened: false,
            server_muted: false,
            server_deafened: false,
            suppressed: false,
            camera: true,
            sharing: false,
            live: false,
        };
        let share_only = Heard { camera: false, sharing: true, ..cam_only };
        assert!(!share_only.camera && share_only.sharing);
        assert!(share_only.sharing || share_only.camera);
    }
}
