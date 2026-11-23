//! Telegram group and channel video chats (`#462` live presence, `#465` scheduled video chats,
//! `#461` joining group voice chats, `#460` join-as channel/peer picker, `#459` invites & links,
//! `#466` start/rename/end with migration, `#464` raise hand, listener/speaker roles & moderation).

use super::peers::Who;
use super::wire::{self, Fault};
use super::{live, sync, Account};
use base64::Engine as _;
use grammers_tl_types as tl;
use protocol::{
    CallFailure, Cue, GroupCallInviteRefusal, GroupCallInviteResult, GroupCallJoinAs,
    GroupCallJoinPeer, GroupCallRtmpUrl, Occupant, Occupants, Participant, PictureKind,
    RtmpStreamState, TelegramGroupOffer, TelegramGroupSource, TelegramGroupVideoSource,
};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Keepalive interval for `phone.checkGroupCall`. Overridable in tests via `TINYCLIENT_TELEGRAM_CHECK_MS`.
fn check_interval() -> Duration {
    if let Ok(ms) = std::env::var("TINYCLIENT_TELEGRAM_CHECK_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            if ms > 0 {
                return Duration::from_millis(ms);
            }
        }
    }
    Duration::from_secs(15)
}

/// Debounce window for coalescing participant volume slider drags into one `editGroupCallParticipant` RPC (#468).
fn gain_debounce() -> Duration {
    if let Ok(ms) = std::env::var("TINYCLIENT_TELEGRAM_GAIN_DEBOUNCE_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return Duration::from_millis(ms);
        }
    }
    Duration::from_millis(250)
}

/// Poll cadence for fetching RTMP live stream segments (`upload.getFile` on `inputGroupCallStream`) (#454).
fn rtmp_poll_interval() -> Duration {
    if let Ok(ms) = std::env::var("TINYCLIENT_TELEGRAM_RTMP_POLL_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            if ms > 0 {
                return Duration::from_millis(ms);
            }
        }
    }
    Duration::from_millis(500)
}

/// Minimum spacing between outgoing `speakingInGroupCallAction` typing updates.
const SPEAKING_PACE: Duration = Duration::from_secs(4);
/// Maximum pages fetched from `phone.getGroupParticipants` on join.
const MAX_PARTICIPANT_PAGES: usize = 5;
/// Participants requested per page.
const PARTICIPANT_PAGE_SIZE: i32 = 50;
/// Safe cap on people invited in a single `phone.inviteToGroupCall` batch (#459).
const MAX_INVITE_BATCH: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GroupVideoEndpoint {
    pub endpoint: String,
    pub ssrcs: Vec<u32>,
    pub paused: bool,
    pub audio_source: Option<u32>,
}

fn extract_video_endpoint(raw: &tl::enums::GroupCallParticipantVideo) -> GroupVideoEndpoint {
    let tl::enums::GroupCallParticipantVideo::Video(v) = raw;
    let mut ssrcs = Vec::new();
    for grp in &v.source_groups {
        let tl::enums::GroupCallParticipantVideoSourceGroup::Group(g) = grp;
        for &src in &g.sources {
            if src > 0 {
                ssrcs.push(src as u32);
            }
        }
    }
    GroupVideoEndpoint {
        endpoint: v.endpoint.clone(),
        ssrcs,
        paused: v.paused,
        audio_source: v.audio_source.filter(|&s| s > 0).map(|s| s as u32),
    }
}

#[derive(Clone, Debug)]
pub(super) struct GroupParticipant {
    pub who: Who,
    pub source: u32,
    pub muted: bool,
    pub can_self_unmute: bool,
    pub speaking: bool,
    pub raise_hand_rating: Option<i64>,
    pub about: String,
    pub muted_by_you: bool,
    pub volume: Option<u16>,
    pub video: Option<GroupVideoEndpoint>,
    pub presentation: Option<GroupVideoEndpoint>,
    pub is_self: bool,
    pub order: u64,
}

impl GroupParticipant {
    pub(super) fn suppressed(&self) -> bool {
        self.muted && !self.can_self_unmute
    }

    pub(super) fn hand_raised(&self) -> bool {
        self.raise_hand_rating.is_some()
    }
}

#[derive(Clone, Debug)]
pub(super) struct TrackedGroupCall {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
    pub participants_count: u32,
    pub schedule_date: Option<i64>,
    pub schedule_start_subscribed: bool,
    pub join_muted: bool,
    pub can_change_join_muted: bool,
    pub record_start_date: Option<i64>,
    pub record_video_active: bool,
    pub can_start_video: bool,
    pub unmuted_video_count: u32,
    pub unmuted_video_limit: u32,
    pub rtmp_stream: bool,
    pub stream_dc_id: Option<i32>,
    pub participants: HashMap<Who, GroupParticipant>,
    pub next_order: u64,
    pub fetched_once: bool,
}

impl TrackedGroupCall {
    fn new(id: i64, access_hash: i64) -> Self {
        Self {
            id,
            access_hash,
            title: String::new(),
            participants_count: 0,
            schedule_date: None,
            schedule_start_subscribed: false,
            join_muted: false,
            can_change_join_muted: false,
            record_start_date: None,
            record_video_active: false,
            can_start_video: true,
            unmuted_video_count: 0,
            unmuted_video_limit: 0,
            rtmp_stream: false,
            stream_dc_id: None,
            participants: HashMap::new(),
            next_order: 0,
            fetched_once: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct PendingGain {
    pub percent: u16,
    pub muted_by_you: bool,
    pub due: Instant,
}

#[derive(Clone, Debug)]
pub(super) struct RtmpViewer {
    pub state: RtmpStreamState,
    pub quality: u8,
    pub qualities: Vec<u8>,
    pub channel_id: Option<i32>,
    pub scale: i32,
    pub next_timestamp_ms: i64,
    pub next_fetch: Instant,
}

#[derive(Clone, Debug)]
pub(super) struct JoinedGroupCall {
    pub who: Who,
    pub call_id: i64,
    pub access_hash: i64,
    pub join_as: Who,
    pub source: u32,
    pub muted: bool,
    pub deafened: bool,
    pub camera_on: bool,
    pub presentation_active: bool,
    pub applied_gains: HashMap<Who, (u16, bool)>,
    pub pending_gains: HashMap<Who, PendingGain>,
    pub rtmp: Option<RtmpViewer>,
    pub next_check: Instant,
    pub last_speaking_sent: Option<Instant>,
}

#[derive(Default)]
pub(super) struct GroupCalls {
    pub calls: HashMap<Who, TrackedGroupCall>,
    pub by_call_id: HashMap<i64, Who>,
    pub joined: Option<JoinedGroupCall>,
}

impl GroupCalls {
    pub(super) fn is_live(&self, who: Who) -> bool {
        self.calls.get(&who).is_some_and(|c| c.schedule_date.is_none())
    }

    pub(super) fn has_call(&self, who: Who) -> bool {
        self.calls.contains_key(&who)
    }
}

fn input_call_of(id: i64, access_hash: i64) -> tl::enums::InputGroupCall {
    tl::types::InputGroupCall { id, access_hash }.into()
}

fn extract_input_call(call: &tl::enums::InputGroupCall) -> Option<(i64, i64)> {
    match call {
        tl::enums::InputGroupCall::Call(c) => Some((c.id, c.access_hash)),
        _ => None,
    }
}

/// Records `chatFull.call` / `channelFull.call` when `info::conversation` or `open_chat` fetches it.
pub(super) fn note_full_call(account: &Account, who: Who, call: Option<&tl::enums::InputGroupCall>) {
    let Some((id, access_hash)) = call.and_then(extract_input_call) else {
        return;
    };
    let mut live = account.live();
    live.group_calls.by_call_id.insert(id, who);
    live.group_calls
        .calls
        .entry(who)
        .and_modify(|c| {
            c.id = id;
            c.access_hash = access_hash;
        })
        .or_insert_with(|| TrackedGroupCall::new(id, access_hash));
}

/// Fetches `InputGroupCall` for `who` via `GetFullChat` / `GetFullChannel` if not yet cached.
fn ensure_input_call(account: &Account, who: Who) -> Result<Option<(i64, i64)>, Fault> {
    if let Some((id, hash)) = account.live().group_calls.calls.get(&who).map(|c| (c.id, c.access_hash)) {
        return Ok(Some((id, hash)));
    }
    let (call, _) = fetch_full_call_info(account, who)?;
    Ok(call)
}

/// Reads `call` and `groupcall_default_join_as` from `GetFullChat` / `GetFullChannel`.
fn fetch_full_call_info(account: &Account, who: Who) -> Result<(Option<(i64, i64)>, Option<Who>), Fault> {
    match who {
        Who::Group(chat_id) => {
            let tl::enums::messages::ChatFull::Full(full) =
                account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
            account.learn(&full.users, &full.chats);
            if let tl::enums::ChatFull::Full(chat) = full.full_chat {
                note_full_call(account, who, chat.call.as_ref());
                let call = chat.call.as_ref().and_then(extract_input_call);
                let default_join_as = chat.groupcall_default_join_as.as_ref().map(Who::of);
                return Ok((call, default_join_as));
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let tl::enums::messages::ChatFull::Full(full) =
                account.ask(&tl::functions::channels::GetFullChannel { channel })?;
            account.learn(&full.users, &full.chats);
            if let tl::enums::ChatFull::ChannelFull(channel) = full.full_chat {
                note_full_call(account, who, channel.call.as_ref());
                let call = channel.call.as_ref().and_then(extract_input_call);
                let default_join_as = channel.groupcall_default_join_as.as_ref().map(Who::of);
                return Ok((call, default_join_as));
            }
        }
        Who::User(_) => {}
    }
    Ok((None, None))
}

fn apply_group_call_obj(tracked: &mut TrackedGroupCall, gc: &tl::types::GroupCall) {
    tracked.id = gc.id;
    tracked.access_hash = gc.access_hash;
    tracked.title = gc.title.clone().unwrap_or_default();
    tracked.participants_count = gc.participants_count.max(0) as u32;
    tracked.schedule_date = gc.schedule_date.filter(|&d| d > 0).map(i64::from);
    tracked.schedule_start_subscribed = gc.schedule_start_subscribed;
    tracked.join_muted = gc.join_muted;
    tracked.can_change_join_muted = gc.can_change_join_muted;
    tracked.record_start_date = gc.record_start_date.filter(|&d| d > 0).map(i64::from);
    tracked.record_video_active = gc.record_video_active;
    tracked.can_start_video = gc.can_start_video;
    tracked.unmuted_video_count = gc.unmuted_video_count.unwrap_or(0).max(0) as u32;
    tracked.unmuted_video_limit = gc.unmuted_video_limit.max(0) as u32;
    tracked.rtmp_stream = gc.rtmp_stream;
    tracked.stream_dc_id = gc.stream_dc_id.filter(|&dc| dc > 0);
}

fn apply_participants(tracked: &mut TrackedGroupCall, list: &[tl::enums::GroupCallParticipant]) {
    for raw in list {
        let tl::enums::GroupCallParticipant::Participant(p) = raw;
        let who = Who::of(&p.peer);
        if p.left {
            if tracked.participants.remove(&who).is_some() {
                tracked.participants_count = tracked.participants_count.saturating_sub(1);
            }
            continue;
        }
        let prev = tracked.participants.get(&who);
        let order = prev.map(|old| old.order).unwrap_or_else(|| {
            let next = tracked.next_order;
            tracked.next_order += 1;
            next
        });
        let about = match &p.about {
            Some(text) => text.clone(),
            None if p.min => prev.map(|old| old.about.clone()).unwrap_or_default(),
            None => String::new(),
        };
        let is_self = p.is_self || (p.min && prev.is_some_and(|old| old.is_self));
        let muted_by_you = if p.min {
            prev.is_some_and(|old| old.muted_by_you)
        } else {
            p.muted_by_you
        };
        let volume = match p.volume {
            Some(v) => Some(protocol::telegram_volume_to_percent(v)),
            None if p.min => prev.and_then(|old| old.volume),
            None => None,
        };
        let video = match p.video.as_ref() {
            Some(v) => Some(extract_video_endpoint(v)),
            None if p.min => prev.and_then(|old| old.video.clone()),
            None => None,
        };
        let presentation = match p.presentation.as_ref() {
            Some(v) => Some(extract_video_endpoint(v)),
            None if p.min => prev.and_then(|old| old.presentation.clone()),
            None => None,
        };
        let source = p.source.max(0) as u32;
        let speaking = !p.muted && p.active_date.is_some();
        let inserted = tracked
            .participants
            .insert(
                who,
                GroupParticipant {
                    who,
                    source,
                    muted: p.muted,
                    can_self_unmute: p.can_self_unmute,
                    speaking,
                    raise_hand_rating: p.raise_hand_rating,
                    about,
                    muted_by_you,
                    volume,
                    video,
                    presentation,
                    is_self,
                    order,
                },
            )
            .is_none();
        if inserted {
            if p.just_joined {
                tracked.participants_count = tracked
                    .participants_count
                    .saturating_add(1)
                    .max(tracked.participants.len() as u32);
            } else if tracked.participants.len() as u32 > tracked.participants_count {
                tracked.participants_count = tracked.participants.len() as u32;
            }
        }
    }
}

/// Roster ordering (`#464`): speakers first, then raised hands (highest rating first), then listeners.
fn sort_participants<'a>(participants: impl Iterator<Item = &'a GroupParticipant>) -> Vec<&'a GroupParticipant> {
    let mut sorted: Vec<&GroupParticipant> = participants.collect();
    sorted.sort_by_key(|p| {
        let tier: u8 = if !p.suppressed() {
            0
        } else if p.hand_raised() {
            1
        } else {
            2
        };
        (
            tier,
            !p.speaking,
            p.muted,
            Reverse(p.raise_hand_rating.unwrap_or(0)),
            p.order,
        )
    });
    sorted
}

fn publish_occupants(account: &Account, who: Who) {
    let (occupants, joined_meta) = {
        let mut live = account.live();
        let Some(tracked) = live.group_calls.calls.get(&who) else {
            drop(live);
            account.ctx.group_call_occupants(&who.text(), Occupants::default());
            sync::refresh_row(account, who);
            return;
        };
        let dir = account.dir();
        let sorted = sort_participants(tracked.participants.values());
        let people: Vec<Occupant> = sorted
            .iter()
            .take(24)
            .map(|p| {
                let id = p.who.text();
                Occupant {
                    person: account.ctx.key(&id),
                    name: dir.name(p.who),
                    avatar: None,
                    muted: p.muted,
                    deafened: false,
                    server_muted: false,
                    server_deafened: false,
                    suppressed: p.suppressed(),
                    speaking: p.speaking,
                    streaming: p.presentation.is_some(),
                    camera: p.video.is_some(),
                    hand_raised: p.hand_raised(),
                    is_channel: !matches!(p.who, Who::User(_)),
                }
            })
            .collect();
        let is_live = tracked.schedule_date.is_none();
        let total = if is_live {
            tracked.participants_count.max(tracked.participants.len() as u32)
        } else {
            0
        };
        let occupants = Occupants {
            people,
            total,
            title: tracked.title.clone(),
            scheduled_at: tracked.schedule_date,
            subscribed: tracked.schedule_start_subscribed,
            live: is_live,
            record_start_date: tracked.record_start_date,
            record_video_active: tracked.record_video_active,
            rtmp_stream: tracked.rtmp_stream,
        };

        let joined_here = live.group_calls.joined.as_ref().is_some_and(|j| j.who == who);
        let joined_meta = if joined_here {
            let self_who = live.group_calls.joined.as_ref().unwrap().join_as;
            let my_participant = tracked
                .participants
                .values()
                .find(|p| p.is_self || p.who == self_who);
            let self_suppressed = tracked.rtmp_stream || my_participant.is_some_and(GroupParticipant::suppressed);
            let self_hand_raised = !tracked.rtmp_stream && my_participant.is_some_and(GroupParticipant::hand_raised);
            let participants: Vec<Participant> = sorted
                .iter()
                .filter(|p| !p.is_self && p.who != self_who)
                .map(|p| Participant {
                    id: p.who.text(),
                    name: dir.name(p.who),
                    muted: p.muted,
                    deafened: false,
                    speaking: p.speaking,
                    suppressed: p.suppressed(),
                    camera: p.video.is_some(),
                    sharing: p.presentation.is_some(),
                    hand_raised: p.hand_raised(),
                    raise_hand_rating: p.raise_hand_rating,
                    about: p.about.clone(),
                    muted_by_you: p.muted_by_you,
                    is_channel: !matches!(p.who, Who::User(_)),
                    volume: p.volume,
                    video_paused: p.video.as_ref().is_some_and(|v| v.paused),
                    presentation_paused: p.presentation.as_ref().is_some_and(|v| v.paused),
                    ..Participant::default()
                })
                .collect();
            let sources: Vec<TelegramGroupSource> = sorted
                .iter()
                .filter(|p| !p.is_self && p.who != self_who && p.source > 0)
                .map(|p| TelegramGroupSource {
                    ssrc: p.source,
                    user_id: p.who.text(),
                })
                .collect();
            let mut video_sources: Vec<TelegramGroupVideoSource> = Vec::new();
            for p in sorted.iter().filter(|p| !p.is_self && p.who != self_who) {
                let uid = p.who.text();
                if let Some(v) = &p.video {
                    video_sources.push(TelegramGroupVideoSource {
                        user_id: uid.clone(),
                        kind: PictureKind::Camera,
                        endpoint: v.endpoint.clone(),
                        ssrcs: v.ssrcs.clone(),
                        paused: v.paused,
                    });
                }
                if let Some(pres) = &p.presentation {
                    video_sources.push(TelegramGroupVideoSource {
                        user_id: uid,
                        kind: PictureKind::Share,
                        endpoint: pres.endpoint.clone(),
                        ssrcs: pres.ssrcs.clone(),
                        paused: pres.paused,
                    });
                }
            }
            let desired_gains: Vec<(Who, u16, bool)> = sorted
                .iter()
                .filter(|p| !p.is_self && p.who != self_who)
                .filter_map(|p| {
                    if p.volume.is_some() || p.muted_by_you {
                        Some((p.who, p.volume.unwrap_or(100), p.muted_by_you))
                    } else {
                        None
                    }
                })
                .collect();
            let rtmp_meta = live
                .group_calls
                .joined
                .as_ref()
                .and_then(|j| j.rtmp.as_ref())
                .map(|r| (r.state, r.quality, r.qualities.clone()));
            let meta = crate::media::GroupCallMeta {
                title: tracked.title.clone(),
                hand_raised: self_hand_raised,
                join_muted: tracked.join_muted,
                can_change_join_muted: tracked.can_change_join_muted,
                participants_count: total,
                joined_as: Some(self_who.text()),
                record_start_date: tracked.record_start_date,
                record_video_active: tracked.record_video_active,
                can_start_video: tracked.can_start_video && !tracked.rtmp_stream,
                unmuted_video_count: tracked.unmuted_video_count,
                unmuted_video_limit: tracked.unmuted_video_limit,
                rtmp_stream: tracked.rtmp_stream,
                rtmp_state: rtmp_meta.as_ref().map(|(s, _, _)| *s),
                rtmp_quality: rtmp_meta.as_ref().map(|(_, q, _)| *q),
                rtmp_qualities: rtmp_meta.map(|(_, _, qs)| qs).unwrap_or_default(),
            };
            let mut gains_to_apply: Vec<(String, u16, bool)> = Vec::new();
            if let Some(joined) = live.group_calls.joined.as_mut() {
                for (p_who, percent, muted_by_you) in desired_gains {
                    if joined.pending_gains.contains_key(&p_who) {
                        continue;
                    }
                    let want = (percent, muted_by_you);
                    if joined.applied_gains.get(&p_who).copied() != Some(want) {
                        joined.applied_gains.insert(p_who, want);
                        gains_to_apply.push((p_who.text(), percent, muted_by_you));
                    }
                }
            }
            Some((participants, sources, video_sources, gains_to_apply, self_suppressed, meta))
        } else {
            None
        };
        (occupants, joined_meta)
    };

    let conv_id = who.text();
    account.ctx.group_call_occupants(&conv_id, occupants);
    sync::refresh_row(account, who);
    if let Some((call_participants, sources, video_sources, gains_to_apply, self_suppressed, meta)) = joined_meta {
        account.ctx.participants(&conv_id, call_participants);
        account.ctx.telegram_group_sources(&conv_id, sources);
        account.ctx.telegram_group_video_sources(&conv_id, video_sources);
        for (uid, percent, muted) in gains_to_apply {
            account.ctx.call_gain(uid, percent, muted);
        }
        account.ctx.self_moderation(&conv_id, false, false, self_suppressed);
        account.ctx.group_call_meta(&conv_id, meta);
    }
}

/// Fetches `phone.getGroupCall` once when a group or channel with an active/scheduled call is opened (`#462`, `#465`).
pub(super) fn open_chat(account: &Account, who: Who) {
    if !matches!(who, Who::Group(_) | Who::Channel(_)) {
        return;
    }
    let active_flag = account.dir().get(who).is_some_and(|k| k.call_active);
    let already_tracked = account.live().group_calls.calls.get(&who).map(|c| c.fetched_once);
    if !active_flag && already_tracked.is_none() {
        return;
    }
    if already_tracked == Some(true) {
        publish_occupants(account, who);
        return;
    }
    let Ok(Some((id, access_hash))) = ensure_input_call(account, who) else {
        return;
    };
    let req = tl::functions::phone::GetGroupCall {
        call: input_call_of(id, access_hash),
        limit: 25,
    };
    let Ok(tl::enums::phone::GroupCall::Call(found)) = account.ask(&req) else {
        return;
    };
    account.learn(&found.users, &found.chats);
    match &found.call {
        tl::enums::GroupCall::Call(gc) => {
            {
                let mut live = account.live();
                live.group_calls.by_call_id.insert(gc.id, who);
                let tracked = live
                    .group_calls
                    .calls
                    .entry(who)
                    .or_insert_with(|| TrackedGroupCall::new(gc.id, gc.access_hash));
                apply_group_call_obj(tracked, gc);
                apply_participants(tracked, &found.participants);
                tracked.fetched_once = true;
            }
            publish_occupants(account, who);
        }
        tl::enums::GroupCall::Discarded(disc) => {
            clear_discarded(account, who, disc.id);
        }
    }
}

fn clear_discarded(account: &Account, who: Who, call_id: i64) {
    let was_joined = {
        let mut live = account.live();
        live.group_calls.calls.remove(&who);
        live.group_calls.by_call_id.remove(&call_id);
        if live.group_calls.joined.as_ref().is_some_and(|j| j.call_id == call_id || j.who == who) {
            live.group_calls.joined = None;
            true
        } else {
            false
        }
    };
    account.dir().set_call_active(who, false, false);
    account.ctx.group_call_occupants(&who.text(), Occupants::default());
    sync::refresh_row(account, who);
    if was_joined {
        account.ctx.call_over(&who.text());
    }
}

fn is_chat_watched(account: &Account, who: Who) -> bool {
    let watching = account.ctx.watching();
    watching
        .conversation
        .as_ref()
        .is_some_and(|open| open.account() == account.ctx.account && open.id == who.text())
}

/// Handles `Update::GroupCall` (`#462`, `#465`, `#461`).
pub(super) fn on_group_call(account: &Account, update: &tl::types::UpdateGroupCall) {
    let call_id = update.call.id();
    let who = update
        .peer
        .as_ref()
        .map(Who::of)
        .or_else(|| account.live().group_calls.by_call_id.get(&call_id).copied());
    let Some(who) = who else {
        return;
    };
    match &update.call {
        tl::enums::GroupCall::Call(gc) => {
            let (need_participants, recording_started) = {
                let mut live = account.live();
                live.group_calls.by_call_id.insert(gc.id, who);
                let joined_here = live.group_calls.joined.as_ref().is_some_and(|j| j.who == who);
                let prev_scheduled = live.group_calls.calls.get(&who).and_then(|c| c.schedule_date);
                let prev_recording = live.group_calls.calls.get(&who).and_then(|c| c.record_start_date);
                let tracked = live
                    .group_calls
                    .calls
                    .entry(who)
                    .or_insert_with(|| TrackedGroupCall::new(gc.id, gc.access_hash));
                apply_group_call_obj(tracked, gc);
                let just_went_live = prev_scheduled.is_some() && tracked.schedule_date.is_none();
                let rec_started = joined_here && prev_recording.is_none() && tracked.record_start_date.is_some();
                ((just_went_live || !tracked.fetched_once) && is_chat_watched(account, who), rec_started)
            };
            account.dir().set_call_active(who, true, gc.participants_count > 0);
            publish_occupants(account, who);
            if recording_started {
                account.ctx.call_cue(&who.text(), Cue::RecordingStarted);
            }
            if need_participants {
                let _ = fetch_participants_pages(account, who, gc.id, gc.access_hash, 1);
            }
        }
        tl::enums::GroupCall::Discarded(disc) => {
            clear_discarded(account, who, disc.id);
        }
    }
}

/// Handles `Update::GroupCallParticipants` (`#462`, `#461`).
pub(super) fn on_participants(account: &Account, update: &tl::types::UpdateGroupCallParticipants) {
    let Some((call_id, _)) = extract_input_call(&update.call) else {
        return;
    };
    let who = account.live().group_calls.by_call_id.get(&call_id).copied();
    let Some(who) = who else {
        return;
    };
    let joined_here = account.live().group_calls.joined.as_ref().is_some_and(|j| j.call_id == call_id);
    if !joined_here && !is_chat_watched(account, who) && !account.live().group_calls.has_call(who) {
        return;
    }
    {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            apply_participants(tracked, &update.participants);
        }
    }
    publish_occupants(account, who);
}

/// Handles `Update::GroupCallConnection` (`#461`, `#467`).
pub(super) fn on_connection(account: &Account, update: &tl::types::UpdateGroupCallConnection) {
    let tl::enums::DataJson::Json(json) = &update.params;
    let conv_id = account.live().group_calls.joined.as_ref().map(|j| j.who.text());
    if let Some(conv_id) = conv_id {
        if update.presentation {
            account.ctx.telegram_group_presentation_answer(&conv_id, json.data.clone());
        } else {
            account.ctx.telegram_group_answer(&conv_id, json.data.clone());
        }
    }
}

/// Reflects `SpeakingInGroupCallAction` from `Update::ChatUserTyping` / `Update::ChannelUserTyping`.
pub(super) fn on_speaking_typing(account: &Account, who: Who, speaker: Who) {
    let changed = {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            if let Some(p) = tracked.participants.get_mut(&speaker) {
                let was = p.speaking;
                p.speaking = true;
                !was
            } else {
                false
            }
        } else {
            false
        }
    };
    if changed {
        publish_occupants(account, who);
    }
}

fn fetch_participants_pages(account: &Account, who: Who, call_id: i64, access_hash: i64, max_pages: usize) -> Result<(), Fault> {
    let mut offset = String::new();
    for _ in 0..max_pages {
        let req = tl::functions::phone::GetGroupParticipants {
            call: input_call_of(call_id, access_hash),
            ids: Vec::new(),
            sources: Vec::new(),
            offset: offset.clone(),
            limit: PARTICIPANT_PAGE_SIZE,
        };
        let tl::enums::phone::GroupParticipants::Participants(page) = account.ask(&req)?;
        account.learn(&page.users, &page.chats);
        let done = page.participants.is_empty() || page.next_offset.is_empty() || page.next_offset == offset;
        {
            let mut live = account.live();
            if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
                tracked.participants_count = tracked.participants_count.max(page.count.max(0) as u32);
                apply_participants(tracked, &page.participants);
                tracked.fetched_once = true;
            }
        }
        offset = page.next_offset;
        if done {
            break;
        }
    }
    publish_occupants(account, who);
    Ok(())
}

// ---------------------------------------------------------------------------
// Schedule, Start, and Subscribe (`#465`)
// ---------------------------------------------------------------------------

fn random_i32() -> i32 {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    i32::from_le_bytes(bytes) & 0x7fff_ffff
}

/// Schedules a Telegram group or channel video chat (`phone.createGroupCall` with `schedule_date`).
pub(super) fn schedule(account: &Account, conversation_id: &str, schedule_date: i64, title: Option<String>) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Video chats can only be scheduled in a group or channel.".to_owned()));
    };
    let peer = account.input(who)?;
    let title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    let answer = account.ask(&tl::functions::phone::CreateGroupCall {
        rtmp_stream: false,
        peer,
        random_id: random_i32(),
        title,
        schedule_date: Some(schedule_date as i32),
    })?;
    live::answered(account, &answer);
    Ok(())
}

/// Starts a scheduled video chat now (`phone.startScheduledGroupCall`) or starts a fresh live video chat.
pub(super) fn start(account: &Account, conversation_id: &str) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Video chats can only be started in a group or channel.".to_owned()));
    };
    let scheduled_call = {
        let live = account.live();
        live.group_calls
            .calls
            .get(&who)
            .filter(|c| c.schedule_date.is_some())
            .map(|c| (c.id, c.access_hash))
    };
    let answer = if let Some((id, access_hash)) = scheduled_call {
        account.ask(&tl::functions::phone::StartScheduledGroupCall {
            call: input_call_of(id, access_hash),
        })?
    } else {
        let peer = account.input(who)?;
        account.ask(&tl::functions::phone::CreateGroupCall {
            rtmp_stream: false,
            peer,
            random_id: random_i32(),
            title: None,
            schedule_date: None,
        })?
    };
    live::answered(account, &answer);
    Ok(())
}

/// Toggles reminder subscription on a scheduled video chat (`phone.toggleGroupCallStartSubscription`).
pub(super) fn subscribe(account: &Account, conversation_id: &str, subscribed: bool) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No scheduled video chat in this conversation.".to_owned()));
    };
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("No scheduled video chat in this conversation.".to_owned()));
    };
    let answer = account.ask(&tl::functions::phone::ToggleGroupCallStartSubscription {
        call: input_call_of(id, access_hash),
        subscribed,
    })?;
    {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            tracked.schedule_start_subscribed = subscribed;
        }
    }
    live::answered(account, &answer);
    publish_occupants(account, who);
    Ok(())
}

// ---------------------------------------------------------------------------
// Join-as, Create (with Migration), Rename, End, Invite, Links, Moderation
// (#460, #459, #466, #464)
// ---------------------------------------------------------------------------

fn require_manage_call(account: &Account, who: Who) -> Result<(), Fault> {
    if account.dir().get(who).is_some_and(|k| k.rights.manage_call) {
        Ok(())
    } else {
        Err(Fault::Refused("You don't have permission to manage video chats in this chat.".to_owned()))
    }
}

fn input_peer_for_identity(account: &Account, identity: Who) -> Result<tl::enums::InputPeer, Fault> {
    if identity == Who::User(account.me) {
        Ok(tl::enums::InputPeer::PeerSelf)
    } else {
        account.input(identity)
    }
}

/// Queries `phone.getGroupCallJoinAs` and reads `groupcall_default_join_as` from the full chat (#460).
pub(super) fn join_as_peers(account: &Account, conversation_id: &str) -> Result<GroupCallJoinAs, Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Video chats are only available in groups and channels.".to_owned()));
    };
    let peer = account.input(who)?;
    let tl::enums::phone::JoinAsPeers::Peers(found) =
        account.ask(&tl::functions::phone::GetGroupCallJoinAs { peer })?;
    account.learn(&found.users, &found.chats);

    let (default_peer, default_unknown) = match fetch_full_call_info(account, who) {
        Ok((_, def)) => (def.map(|w| w.text()), false),
        Err(_) => (None, true),
    };

    let dir = account.dir();
    let me = Who::User(account.me);
    let mut seen = HashSet::new();
    let mut peers = Vec::with_capacity(found.peers.len());
    for raw in &found.peers {
        let p_who = Who::of(raw);
        if !seen.insert(p_who) {
            continue;
        }
        let is_self = p_who == me;
        let name = if is_self { "Me".to_owned() } else { dir.name(p_who) };
        peers.push(GroupCallJoinPeer {
            id: p_who.text(),
            name,
            is_self,
            avatar: None,
        });
    }
    Ok(GroupCallJoinAs { peers, default_peer, default_unknown })
}

/// Saves the remembered join-as peer for a chat (`phone.saveDefaultGroupCallJoinAs`) (#460).
pub(super) fn save_default_join_as(account: &Account, conversation_id: &str, peer_id: &str) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Video chats are only available in groups and channels.".to_owned()));
    };
    let Some((join_who, None)) = Who::parse(peer_id) else {
        return Err(Fault::Refused("Unknown join-as identity.".to_owned()));
    };
    let peer = account.input(who)?;
    let join_as = input_peer_for_identity(account, join_who)?;
    let _ = account.ask(&tl::functions::phone::SaveDefaultGroupCallJoinAs { peer, join_as })?;
    Ok(())
}

/// Extracts a migrated channel id from `Updates` if Telegram upgraded a basic group to a supergroup.
fn migrated_channel_in_updates(updates: &tl::enums::Updates, original: Who) -> Option<Who> {
    if !matches!(original, Who::Group(_)) {
        return None;
    }
    let list = match updates {
        tl::enums::Updates::Updates(u) => u.updates.as_slice(),
        tl::enums::Updates::Combined(u) => u.updates.as_slice(),
        tl::enums::Updates::UpdateShort(u) => std::slice::from_ref(&u.update),
        _ => &[],
    };
    for upd in list {
        match upd {
            tl::enums::Update::NewMessage(m) => {
                if let tl::enums::Message::Service(svc) = &m.message {
                    if let tl::enums::MessageAction::ChatMigrateTo(mig) = &svc.action {
                        return Some(Who::Channel(mig.channel_id));
                    }
                }
            }
            tl::enums::Update::NewChannelMessage(m) => {
                if let tl::enums::Message::Service(svc) = &m.message {
                    if let tl::enums::MessageAction::ChatMigrateTo(mig) = &svc.action {
                        return Some(Who::Channel(mig.channel_id));
                    }
                }
            }
            tl::enums::Update::GroupCall(gc) => {
                if let Some(peer) = gc.peer.as_ref() {
                    let who = Who::of(peer);
                    if matches!(who, Who::Channel(_)) {
                        return Some(who);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Starts a live Telegram group or channel video chat (`phone.createGroupCall`), following
/// basic-group-to-supergroup migration if Telegram upgrades the chat, and optionally joining (#466).
pub(super) fn create(
    account: &Account,
    conversation_id: &str,
    title: Option<String>,
    rtmp_stream: bool,
    join: bool,
) -> Result<String, Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Video chats can only be started in a group or channel.".to_owned()));
    };
    require_manage_call(account, who)?;
    let clean_title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    let peer = account.input(who)?;
    let req = tl::functions::phone::CreateGroupCall {
        rtmp_stream,
        peer,
        random_id: random_i32(),
        title: clean_title.clone(),
        schedule_date: None,
    };
    let (target_who, answer) = match account.ask(&req) {
        Ok(ans) => {
            let target = migrated_channel_in_updates(&ans, who).unwrap_or(who);
            (target, ans)
        }
        Err(fault)
            if matches!(who, Who::Group(_))
                && (fault.is("CHAT_MIGRATED")
                    || fault.is("MIGRATE_TO_CHANNEL")
                    || fault.is("GROUPCALL_MIGRATE")) =>
        {
            let Who::Group(chat_id) = who else { unreachable!() };
            let migrated = account.ask(&tl::functions::messages::MigrateChat { chat_id })?;
            live::answered(account, &migrated);
            let new_who = migrated_channel_in_updates(&migrated, who).ok_or_else(|| {
                Fault::Refused("Group upgraded to a supergroup, but the new chat could not be found.".to_owned())
            })?;
            let new_peer = account.input(new_who)?;
            let ans = account.ask(&tl::functions::phone::CreateGroupCall {
                rtmp_stream,
                peer: new_peer,
                random_id: random_i32(),
                title: clean_title,
                schedule_date: None,
            })?;
            (new_who, ans)
        }
        Err(fault) => return Err(fault),
    };
    live::answered(account, &answer);
    if target_who != who {
        sync::refresh_row(account, target_who);
    }
    if join {
        join_group_with(account, &target_who.text(), None, None, false)?;
    }
    Ok(target_who.text())
}

/// Renames a live or scheduled video chat (`phone.editGroupCallTitle`) (#466).
pub(super) fn edit_title(account: &Account, conversation_id: &str, title: &str) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    require_manage_call(account, who)?;
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };
    let clean = title.trim().to_owned();
    let answer = account.ask(&tl::functions::phone::EditGroupCallTitle {
        call: input_call_of(id, access_hash),
        title: clean.clone(),
    })?;
    {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            tracked.title = clean;
        }
    }
    live::answered(account, &answer);
    publish_occupants(account, who);
    Ok(())
}

/// Ends a Telegram video chat for everyone (`phone.discardGroupCall`, requires `manage_call`) (#466).
pub(super) fn discard(account: &Account, conversation_id: &str) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    require_manage_call(account, who)?;
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };
    let answer = account.ask(&tl::functions::phone::DiscardGroupCall {
        call: input_call_of(id, access_hash),
    })?;
    live::answered(account, &answer);
    clear_discarded(account, who, id);
    Ok(())
}

fn invite_refusal_words(fault: &Fault) -> String {
    if fault.is("USER_PRIVACY_RESTRICTED") {
        "Privacy settings prevent inviting this person.".to_owned()
    } else if fault.is("USER_NOT_PARTICIPANT") {
        "This person is not a member of this chat.".to_owned()
    } else if fault.is("USER_NOT_MUTUAL_CONTACT") {
        "Only mutual contacts can be invited.".to_owned()
    } else if fault.is("USER_BANNED_IN_CHANNEL") || fault.is("USER_KICKED") {
        "This person is banned from this chat.".to_owned()
    } else if fault.is("USER_ALREADY_PARTICIPANT") {
        "Already in the video chat.".to_owned()
    } else if fault.is("INPUT_USER_DEACTIVATED") {
        "This account is deactivated.".to_owned()
    } else {
        fault.words()
    }
}

fn is_per_user_invite_fault(fault: &Fault) -> bool {
    fault.is("USER_PRIVACY_RESTRICTED")
        || fault.is("USER_NOT_PARTICIPANT")
        || fault.is("USER_NOT_MUTUAL_CONTACT")
        || fault.is("USER_BANNED_IN_CHANNEL")
        || fault.is("USER_KICKED")
        || fault.is("USER_ALREADY_PARTICIPANT")
        || fault.is("INPUT_USER_DEACTIVATED")
}

/// Invites explicitly selected users into a live video chat (`phone.inviteToGroupCall`) (#459).
pub(super) fn invite(
    account: &Account,
    conversation_id: &str,
    users: Vec<String>,
) -> Result<GroupCallInviteResult, Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Video chats are only available in groups and channels.".to_owned()));
    };
    if users.is_empty() {
        return Err(Fault::Refused("Select at least one person to invite.".to_owned()));
    }
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };

    let mut result = GroupCallInviteResult::default();
    let mut resolved: Vec<(String, tl::enums::InputUser)> = Vec::new();
    let mut seen = HashSet::new();
    for raw_id in users.into_iter().take(MAX_INVITE_BATCH) {
        if !seen.insert(raw_id.clone()) {
            continue;
        }
        let Some((u_who @ Who::User(_), None)) = Who::parse(&raw_id) else {
            result.refused.push(GroupCallInviteRefusal {
                user_id: raw_id,
                reason: "Only user accounts can be invited to a video chat.".to_owned(),
            });
            continue;
        };
        let _ = account.input(u_who);
        match account.dir().input_user(u_who) {
            Some(input_user) => resolved.push((raw_id, input_user)),
            None => result.refused.push(GroupCallInviteRefusal {
                user_id: raw_id,
                reason: "This person is not known to this chat.".to_owned(),
            }),
        }
    }
    if resolved.is_empty() {
        return Ok(result);
    }

    let req = tl::functions::phone::InviteToGroupCall {
        call: input_call_of(id, access_hash),
        users: resolved.iter().map(|(_, u)| u.clone()).collect(),
    };
    match account.ask(&req) {
        Ok(answer) => {
            live::answered(account, &answer);
            result.invited.extend(resolved.into_iter().map(|(uid, _)| uid));
            Ok(result)
        }
        Err(fault) if is_per_user_invite_fault(&fault) => {
            if resolved.len() == 1 {
                let (uid, _) = resolved.remove(0);
                result.refused.push(GroupCallInviteRefusal {
                    user_id: uid,
                    reason: invite_refusal_words(&fault),
                });
                return Ok(result);
            }
            for (uid, input_user) in resolved {
                let single = tl::functions::phone::InviteToGroupCall {
                    call: input_call_of(id, access_hash),
                    users: vec![input_user],
                };
                match account.ask(&single) {
                    Ok(answer) => {
                        live::answered(account, &answer);
                        result.invited.push(uid);
                    }
                    Err(single_fault) => {
                        result.refused.push(GroupCallInviteRefusal {
                            user_id: uid,
                            reason: invite_refusal_words(&single_fault),
                        });
                    }
                }
            }
            Ok(result)
        }
        Err(fault) => Err(fault),
    }
}

/// Exports a listener link or (for admins) speaker link (`phone.exportGroupCallInvite`) (#459).
pub(super) fn export_invite(
    account: &Account,
    conversation_id: &str,
    can_self_unmute: bool,
) -> Result<String, Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    if can_self_unmute {
        require_manage_call(account, who)?;
    }
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };
    let tl::enums::phone::ExportedGroupCallInvite::Invite(exported) =
        account.ask(&tl::functions::phone::ExportGroupCallInvite {
            can_self_unmute,
            call: input_call_of(id, access_hash),
        })?;
    Ok(exported.link)
}

/// Updates admin group call settings (`phone.toggleGroupCallSettings`: `join_muted` / `reset_invite_hash`) (#459, #464).
pub(super) fn settings(
    account: &Account,
    conversation_id: &str,
    join_muted: Option<bool>,
    reset_invite_hash: bool,
) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    require_manage_call(account, who)?;
    if join_muted.is_some()
        && account
            .live()
            .group_calls
            .calls
            .get(&who)
            .is_some_and(|c| !c.can_change_join_muted)
    {
        return Err(Fault::Refused("This video chat does not allow changing join-muted settings.".to_owned()));
    }
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };
    let answer = account.ask(&tl::functions::phone::ToggleGroupCallSettings {
        reset_invite_hash,
        call: input_call_of(id, access_hash),
        join_muted,
        messages_enabled: None,
        send_paid_messages_stars: None,
    })?;
    if let Some(jm) = join_muted {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            tracked.join_muted = jm;
        }
    }
    live::answered(account, &answer);
    publish_occupants(account, who);
    Ok(())
}

/// Raises or lowers our own hand in a moderated Telegram video chat (`phone.editGroupCallParticipant`) (#464).
pub(super) fn raise_hand(account: &Account, conversation_id: &str, raised: bool) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    let joined = account
        .live()
        .group_calls
        .joined
        .as_ref()
        .filter(|j| j.who == who)
        .map(|j| (j.call_id, j.access_hash, j.join_as));
    let Some((call_id, access_hash, join_as)) = joined else {
        return Err(Fault::Refused("Join the video chat before raising your hand.".to_owned()));
    };
    let participant = input_peer_for_identity(account, join_as)?;
    let answer = account.ask(&tl::functions::phone::EditGroupCallParticipant {
        call: input_call_of(call_id, access_hash),
        participant,
        muted: None,
        volume: None,
        raise_hand: Some(raised),
        video_stopped: None,
        video_paused: None,
        presentation_paused: None,
    })?;
    live::answered(account, &answer);
    {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            if let Some(me) = tracked.participants.get_mut(&join_as) {
                if raised && me.raise_hand_rating.is_none() {
                    me.raise_hand_rating = Some(1);
                } else if !raised {
                    me.raise_hand_rating = None;
                }
            }
        }
    }
    publish_occupants(account, who);
    Ok(())
}

/// Admin participant moderation (`manage_call`): mute, allow to speak, or remove/ban from group (#464).
pub(super) fn moderate(
    account: &Account,
    conversation_id: &str,
    participant_id: &str,
    muted: Option<bool>,
    remove: bool,
) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    require_manage_call(account, who)?;
    let Some((target_who, None)) = Who::parse(participant_id) else {
        return Err(Fault::Refused("Unknown participant.".to_owned()));
    };
    if remove {
        let answer = match who {
            Who::Channel(_) => {
                let channel = account.input_channel(who)?;
                let participant = account.input(target_who)?;
                let banned_rights = tl::types::ChatBannedRights {
                    view_messages: true,
                    send_messages: true,
                    send_media: true,
                    send_stickers: true,
                    send_gifs: true,
                    send_games: true,
                    send_inline: true,
                    embed_links: true,
                    send_polls: true,
                    change_info: true,
                    invite_users: true,
                    pin_messages: true,
                    manage_topics: true,
                    send_photos: true,
                    send_videos: true,
                    send_roundvideos: true,
                    send_audios: true,
                    send_voices: true,
                    send_docs: true,
                    send_plain: true,
                    send_reactions: true,
                    edit_rank: true,
                    until_date: 0,
                }
                .into();
                account.ask(&tl::functions::channels::EditBanned {
                    channel,
                    participant,
                    banned_rights,
                })?
            }
            Who::Group(chat_id) => {
                let Who::User(_) = target_who else {
                    return Err(Fault::Refused("Only user accounts can be removed from a basic group.".to_owned()));
                };
                let _ = account.input(target_who);
                let user_id = account
                    .dir()
                    .input_user(target_who)
                    .ok_or_else(|| Fault::Refused("Unknown user.".to_owned()))?;
                account.ask(&tl::functions::messages::DeleteChatUser {
                    revoke_history: false,
                    chat_id,
                    user_id,
                })?
            }
            Who::User(_) => unreachable!(),
        };
        live::answered(account, &answer);
        {
            let mut live = account.live();
            if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
                if tracked.participants.remove(&target_who).is_some() {
                    tracked.participants_count = tracked.participants_count.saturating_sub(1);
                }
            }
        }
        publish_occupants(account, who);
        return Ok(());
    }

    let Some(mute_val) = muted else {
        return Err(Fault::Refused("No moderation action specified.".to_owned()));
    };
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };
    let participant = account.input(target_who)?;
    let answer = account.ask(&tl::functions::phone::EditGroupCallParticipant {
        call: input_call_of(id, access_hash),
        participant,
        muted: Some(mute_val),
        volume: None,
        raise_hand: None,
        video_stopped: None,
        video_paused: None,
        presentation_paused: None,
    })?;
    live::answered(account, &answer);
    publish_occupants(account, who);
    Ok(())
}

// ---------------------------------------------------------------------------
// Recording (#463), Per-person Gain (#468), Camera/Share (#467), RTMP (#454)
// ---------------------------------------------------------------------------

/// Starts or stops Telegram server-side recording (`phone.toggleGroupCallRecord`, requires `manage_call`) (#463).
pub(super) fn toggle_record(
    account: &Account,
    conversation_id: &str,
    start: bool,
    video: bool,
    video_portrait: Option<bool>,
    title: Option<String>,
) -> Result<(bool, Option<String>), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No video chat in this conversation.".to_owned()));
    };
    require_manage_call(account, who)?;
    let Some((id, access_hash)) = ensure_input_call(account, who)? else {
        return Err(Fault::Refused("There is no active video chat in this chat.".to_owned()));
    };
    let clean_title = if start {
        title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
    } else {
        None
    };
    let req = tl::functions::phone::ToggleGroupCallRecord {
        start,
        video: start && video,
        call: input_call_of(id, access_hash),
        title: clean_title,
        video_portrait: if start && video { Some(video_portrait.unwrap_or(false)) } else { None },
    };
    let answer = account.ask(&req)?;
    live::answered(account, &answer);
    {
        let mut live = account.live();
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            if !start {
                tracked.record_start_date = None;
                tracked.record_video_active = false;
            } else if tracked.record_start_date.is_none() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(1);
                tracked.record_start_date = Some(now);
                tracked.record_video_active = video;
            }
        }
    }
    publish_occupants(account, who);
    let notice = if !start {
        Some("Recording stopped. The file will arrive in your Saved Messages.".to_owned())
    } else {
        None
    };
    Ok((start, notice))
}

/// Coalesces local participant volume / mute-for-me changes into a debounced `phone.editGroupCallParticipant` (#468).
pub(super) fn queue_participant_gain(
    account: &Account,
    conversation_id: &str,
    participant_id: &str,
    percent: u16,
    muted_by_you: bool,
) {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return;
    };
    let Some((target_who, None)) = Who::parse(participant_id) else {
        return;
    };
    let percent = percent.min(protocol::GAIN_MAX);
    let updated = {
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut().filter(|j| j.who == who) else {
            return;
        };
        joined.applied_gains.insert(target_who, (percent, muted_by_you));
        joined.pending_gains.insert(
            target_who,
            PendingGain {
                percent,
                muted_by_you,
                due: Instant::now() + gain_debounce(),
            },
        );
        if let Some(tracked) = live.group_calls.calls.get_mut(&who) {
            if let Some(p) = tracked.participants.get_mut(&target_who) {
                p.volume = Some(percent);
                p.muted_by_you = muted_by_you;
            }
        }
        true
    };
    if updated {
        publish_occupants(account, who);
    }
}

/// Flushes due debounced participant volume/mute-for-me edits to Telegram (#468).
/// CRITICAL: When the caller has `manage_call`, Telegram treats `muted: Some(true)` as
/// "Mute for everyone". Therefore an admin's local "Mute for me" NEVER sends `muted: Some(true)` —
/// it stays local in `mediad` (`MediaRequest::Gain`) and only syncs `volume` safely.
fn flush_pending_gains(account: &Account) {
    let due_items: Vec<(Who, i64, i64, Who, PendingGain)> = {
        let now = Instant::now();
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        let ready: Vec<Who> = joined
            .pending_gains
            .iter()
            .filter_map(|(&p_who, g)| (g.due <= now).then_some(p_who))
            .collect();
        let mut out = Vec::with_capacity(ready.len());
        for p_who in ready {
            if let Some(g) = joined.pending_gains.remove(&p_who) {
                out.push((joined.who, joined.call_id, joined.access_hash, p_who, g));
            }
        }
        out
    };
    for (who, call_id, access_hash, target_who, g) in due_items {
        let manage = account.dir().get(who).is_some_and(|k| k.rights.manage_call);
        let Ok(participant) = account.input(target_who) else {
            continue;
        };
        let (muted, volume) = if manage {
            // Admin local mute MUST NEVER send `muted: Some(true)` (which mutes for everyone on Telegram).
            let vol = if g.muted_by_you {
                1
            } else {
                protocol::percent_to_telegram_volume(g.percent)
            };
            (None, Some(vol))
        } else {
            (
                Some(g.muted_by_you),
                Some(protocol::percent_to_telegram_volume(g.percent)),
            )
        };
        let req = tl::functions::phone::EditGroupCallParticipant {
            call: input_call_of(call_id, access_hash),
            participant,
            muted,
            volume,
            raise_hand: None,
            video_stopped: None,
            video_paused: None,
            presentation_paused: None,
        };
        if let Ok(answer) = wire::call(account.wire.as_ref(), &req) {
            live::answered(account, &answer);
        }
    }
}

/// Reports local camera state to Telegram (`video_stopped` / `video_paused`) (#467).
pub(super) fn on_camera(account: &Account, on: bool) {
    let target = {
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        if joined.rtmp.is_some() {
            return;
        }
        joined.camera_on = on;
        (joined.who, joined.call_id, joined.access_hash, joined.join_as)
    };
    let (who, call_id, access_hash, join_as) = target;
    let Ok(participant) = input_peer_for_identity(account, join_as) else {
        return;
    };
    let req = tl::functions::phone::EditGroupCallParticipant {
        call: input_call_of(call_id, access_hash),
        participant,
        muted: None,
        volume: None,
        raise_hand: None,
        video_stopped: Some(!on),
        video_paused: Some(false),
        presentation_paused: None,
    };
    if let Ok(answer) = wire::call(account.wire.as_ref(), &req) {
        live::answered(account, &answer);
    }
    publish_occupants(account, who);
}

/// Starts or stops Telegram screen share (`phone.joinGroupCallPresentation` / `leaveGroupCallPresentation`) (#467).
pub(super) fn on_share(account: &Account, on: bool) {
    let info = {
        let live = account.live();
        live.group_calls
            .joined
            .as_ref()
            .filter(|j| j.rtmp.is_none())
            .map(|j| (j.who, j.call_id, j.access_hash, j.join_as, j.presentation_active))
    };
    let Some((who, call_id, access_hash, join_as, was_active)) = info else {
        return;
    };
    let conv_id = who.text();
    if on {
        let Ok((params_json, _ssrc)) = account.ctx.telegram_group_presentation_offer(&conv_id) else {
            return;
        };
        let req = tl::functions::phone::JoinGroupCallPresentation {
            call: input_call_of(call_id, access_hash),
            params: tl::types::DataJson { data: params_json }.into(),
        };
        match wire::call(account.wire.as_ref(), &req) {
            Ok(answer) => {
                if let Some(joined) = account.live().group_calls.joined.as_mut() {
                    if joined.who == who {
                        joined.presentation_active = true;
                    }
                }
                live::answered(account, &answer);
                if let Ok(participant) = input_peer_for_identity(account, join_as) {
                    let unpause = tl::functions::phone::EditGroupCallParticipant {
                        call: input_call_of(call_id, access_hash),
                        participant,
                        muted: None,
                        volume: None,
                        raise_hand: None,
                        video_stopped: None,
                        video_paused: None,
                        presentation_paused: Some(false),
                    };
                    if let Ok(ans) = wire::call(account.wire.as_ref(), &unpause) {
                        live::answered(account, &ans);
                    }
                }
            }
            Err(_) => {
                account.ctx.telegram_group_presentation_leave(&conv_id);
            }
        }
    } else if was_active {
        if let Some(joined) = account.live().group_calls.joined.as_mut() {
            if joined.who == who {
                joined.presentation_active = false;
            }
        }
        account.ctx.telegram_group_presentation_leave(&conv_id);
        let req = tl::functions::phone::LeaveGroupCallPresentation {
            call: input_call_of(call_id, access_hash),
        };
        if let Ok(answer) = wire::call(account.wire.as_ref(), &req) {
            live::answered(account, &answer);
        }
    }
    publish_occupants(account, who);
}

/// Queries or revokes the RTMP server URL & stream key (`phone.getGroupCallStreamRtmpUrl`, requires `manage_call`) (#454).
/// Never logs the returned stream key.
pub(super) fn rtmp_url(
    account: &Account,
    conversation_id: &str,
    revoke: bool,
) -> Result<GroupCallRtmpUrl, Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("Live stream keys are only available for groups and channels.".to_owned()));
    };
    require_manage_call(account, who)?;
    let peer = account.input(who)?;
    let tl::enums::phone::GroupCallStreamRtmpUrl::Url(found) = account.ask(
        &tl::functions::phone::GetGroupCallStreamRtmpUrl {
            live_story: false,
            peer,
            revoke,
        },
    )?;
    Ok(GroupCallRtmpUrl { url: found.url, key: found.key })
}

/// Switches the requested quality tier (0 = Low, 1 = Medium, 2 = High) on a watched RTMP live stream (#454).
pub(super) fn set_rtmp_quality(
    account: &Account,
    conversation_id: &str,
    quality: u8,
) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        return Err(Fault::Refused("No live stream in this conversation.".to_owned()));
    };
    let clamped = quality.min(2);
    {
        let mut live = account.live();
        let Some(rtmp) = live
            .group_calls
            .joined
            .as_mut()
            .filter(|j| j.who == who)
            .and_then(|j| j.rtmp.as_mut())
        else {
            return Err(Fault::Refused("Not currently watching a live stream in this chat.".to_owned()));
        };
        rtmp.quality = clamped;
        rtmp.next_fetch = Instant::now();
    }
    publish_occupants(account, who);
    tick_rtmp(account);
    Ok(())
}

/// Polls `phone.getGroupCallStreamChannels` and fetches `inputGroupCallStream` segments via `upload.getFile`
/// on `stream_dc_id` while watching an RTMP live stream (#454).
fn tick_rtmp(account: &Account) {
    let due = {
        let now = Instant::now();
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        let Some(rtmp) = joined.rtmp.as_mut() else {
            return;
        };
        if now < rtmp.next_fetch || rtmp.state == RtmpStreamState::Ended {
            return;
        }
        rtmp.next_fetch = now + rtmp_poll_interval();
        let who = joined.who;
        let call_id = joined.call_id;
        let access_hash = joined.access_hash;
        let channel_id = rtmp.channel_id;
        let scale = rtmp.scale;
        let next_ts = rtmp.next_timestamp_ms;
        let quality = rtmp.quality;
        let state = rtmp.state;
        let stream_dc = live.group_calls.calls.get(&who).and_then(|c| c.stream_dc_id);
        (who, call_id, access_hash, stream_dc, channel_id, scale, next_ts, quality, state)
    };
    let (who, call_id, access_hash, stream_dc, mut channel_id, mut scale, mut ts_ms, quality, prev_state) = due;
    let conv_id = who.text();
    let mut new_state = prev_state;

    if channel_id.is_none() || matches!(prev_state, RtmpStreamState::Waiting | RtmpStreamState::Paused) {
        let req = tl::functions::phone::GetGroupCallStreamChannels {
            call: input_call_of(call_id, access_hash),
        };
        match wire::call_in(account.wire.as_ref(), stream_dc, &req) {
            Ok(tl::enums::phone::GroupCallStreamChannels::Channels(found)) => {
                if let Some(tl::enums::GroupCallStreamChannel::Channel(ch)) = found.channels.first() {
                    channel_id = Some(ch.channel);
                    scale = ch.scale;
                    if ts_ms == 0 || ts_ms < ch.last_timestamp_ms {
                        ts_ms = ch.last_timestamp_ms;
                    }
                    new_state = RtmpStreamState::Live;
                } else {
                    new_state = RtmpStreamState::Paused;
                }
            }
            Err(fault) if fault.is("GROUPCALL_INVALID") => {
                new_state = RtmpStreamState::Ended;
            }
            Err(_) => {
                new_state = RtmpStreamState::Paused;
            }
        }
    }

    let mut next_ts = ts_ms;
    if let Some(ch) = channel_id.filter(|_| new_state != RtmpStreamState::Ended) {
        let loc = tl::types::InputGroupCallStream {
            call: input_call_of(call_id, access_hash),
            time_ms: ts_ms,
            scale,
            video_channel: Some(ch),
            video_quality: Some(i32::from(quality)),
        };
        let req = tl::functions::upload::GetFile {
            precise: false,
            cdn_supported: false,
            location: loc.into(),
            offset: 0,
            limit: 512 * 1024,
        };
        match wire::call_in(account.wire.as_ref(), stream_dc, &req) {
            Ok(tl::enums::upload::File::File(piece)) => {
                if !piece.bytes.is_empty() {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&piece.bytes);
                    account.ctx.telegram_rtmp_segment(
                        &conv_id,
                        ts_ms,
                        scale,
                        Some(ch),
                        Some(quality),
                        b64,
                    );
                    new_state = RtmpStreamState::Live;
                    let step_ms = (1000i64 >> scale.clamp(0, 4)).max(250);
                    next_ts = ts_ms.saturating_add(step_ms);
                } else {
                    new_state = RtmpStreamState::Paused;
                }
            }
            Ok(tl::enums::upload::File::CdnRedirect(_)) => {
                new_state = RtmpStreamState::Paused;
            }
            Err(fault) if fault.is("GROUPCALL_INVALID") => {
                new_state = RtmpStreamState::Ended;
            }
            Err(_) => {
                // TIME_TOO_BIG / TIME_TOO_SMALL / GROUPCALL_STREAM_NOT_READY -> paused; re-query channels next tick.
                new_state = RtmpStreamState::Paused;
                channel_id = None;
            }
        }
    }

    {
        let mut live = account.live();
        if let Some(rtmp) = live
            .group_calls
            .joined
            .as_mut()
            .filter(|j| j.who == who)
            .and_then(|j| j.rtmp.as_mut())
        {
            rtmp.channel_id = channel_id;
            rtmp.scale = scale;
            rtmp.next_timestamp_ms = next_ts;
            rtmp.state = new_state;
        }
    }
    if new_state != prev_state {
        account.ctx.telegram_rtmp_state(&conv_id, new_state);
        publish_occupants(account, who);
    }
}

// ---------------------------------------------------------------------------
// Join, Audio Mute, Speaking, Keepalive, Leave (`#461`, `#460`, `#459`)
// ---------------------------------------------------------------------------

/// Joins a live Telegram group or channel voice chat (`Command::CallJoin` on `g<id>` or `c<id>`).
pub(super) fn join_group(account: &Account, who: Who) {
    let _ = join_group_with(account, &who.text(), None, None, false);
}

/// Joins (or switches identity mid-call in) a Telegram group/channel video chat with explicit
/// `join_as`, `invite_hash`, and optional `save_default` (#460, #459).
pub(super) fn join_group_with(
    account: &Account,
    conversation_id: &str,
    join_as: Option<String>,
    invite_hash: Option<String>,
    save_default: bool,
) -> Result<(), Fault> {
    let Some((who @ (Who::Group(_) | Who::Channel(_)), None)) = Who::parse(conversation_id) else {
        let msg = "Video chats are only available in groups and channels.";
        account.ctx.call_failed(conversation_id, CallFailure::Refused, msg);
        return Err(Fault::Refused(msg.to_owned()));
    };
    let conv_id = who.text();
    let join_as_who = match join_as {
        Some(ref pid) if !pid.is_empty() => match Who::parse(pid) {
            Some((parsed, None)) => parsed,
            _ => {
                let msg = "Unknown join-as identity.";
                account.ctx.call_failed(&conv_id, CallFailure::Refused, msg);
                return Err(Fault::Refused(msg.to_owned()));
            }
        },
        _ => Who::User(account.me),
    };
    let input_join_as = match input_peer_for_identity(account, join_as_who) {
        Ok(p) => p,
        Err(fault) => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            return Err(fault);
        }
    };

    if save_default {
        if let Ok(peer) = account.input(who) {
            let _ = account.ask(&tl::functions::phone::SaveDefaultGroupCallJoinAs {
                peer,
                join_as: input_join_as.clone(),
            });
        }
    }

    let call_ref = match ensure_input_call(account, who) {
        Ok(Some(c)) => c,
        Ok(None) => {
            let msg = "There is no live video chat in this chat.";
            account.ctx.call_failed(&conv_id, CallFailure::Refused, msg);
            return Err(Fault::Refused(msg.to_owned()));
        }
        Err(fault) => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            return Err(fault);
        }
    };
    // Ensure we know whether the call is live (not scheduled-only) and whether it is an RTMP stream.
    let (call_id, access_hash) = call_ref;
    let is_rtmp = {
        let mut tracked_opt = account
            .live()
            .group_calls
            .calls
            .get(&who)
            .map(|c| (c.schedule_date.is_some(), c.rtmp_stream, c.fetched_once));
        if tracked_opt.is_none_or(|(_, _, fetched)| !fetched) {
            if let Ok(tl::enums::phone::GroupCall::Call(found)) = account.ask(&tl::functions::phone::GetGroupCall {
                call: input_call_of(call_id, access_hash),
                limit: 10,
            }) {
                account.learn(&found.users, &found.chats);
                if let tl::enums::GroupCall::Call(gc) = &found.call {
                    let mut live = account.live();
                    live.group_calls.by_call_id.insert(gc.id, who);
                    let tracked = live
                        .group_calls
                        .calls
                        .entry(who)
                        .or_insert_with(|| TrackedGroupCall::new(gc.id, gc.access_hash));
                    apply_group_call_obj(tracked, gc);
                    apply_participants(tracked, &found.participants);
                    tracked.fetched_once = true;
                    tracked_opt = Some((tracked.schedule_date.is_some(), tracked.rtmp_stream, true));
                }
            }
        }
        if tracked_opt.is_some_and(|(sched, _, _)| sched) {
            let msg = "This video chat hasn't started yet.";
            account.ctx.call_failed(&conv_id, CallFailure::Refused, msg);
            return Err(Fault::Refused(msg.to_owned()));
        }
        tracked_opt.is_some_and(|(_, rtmp, _)| rtmp)
    };

    // Mid-call identity switch: cleanly leave the existing participant slot before rejoining.
    let switching_here = account
        .live()
        .group_calls
        .joined
        .as_ref()
        .is_some_and(|j| j.who == who);
    if switching_here {
        leave_group(account, who);
    }

    let (params_json, ssrc, rtmp_viewer) = if is_rtmp {
        if let Err(detail) = account.ctx.telegram_rtmp_start(&conv_id) {
            account.ctx.call_failed(&conv_id, CallFailure::Engine, &detail);
            return Err(Fault::Refused(detail));
        }
        let viewer_params = serde_json::json!({
            "ufrag": "rtmp",
            "pwd": "rtmpviewerpassword123456",
            "fingerprints": [{
                "hash": "sha-256",
                "setup": "active",
                "fingerprint": "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00"
            }],
            "ssrc": 1
        })
        .to_string();
        (
            viewer_params,
            1u32,
            Some(RtmpViewer {
                state: RtmpStreamState::Waiting,
                quality: 1,
                qualities: vec![0, 1, 2],
                channel_id: None,
                scale: 0,
                next_timestamp_ms: 0,
                next_fetch: Instant::now(),
            }),
        )
    } else {
        // Always join muted by default (`#461`).
        let (params_json, ssrc) = match account.ctx.telegram_group_offer(
            &conv_id,
            TelegramGroupOffer {
                user_id: join_as_who.text(),
                muted: true,
                deafened: false,
            },
        ) {
            Ok(pair) => pair,
            Err(detail) => {
                account.ctx.call_failed(&conv_id, CallFailure::Engine, &detail);
                return Err(Fault::Refused(detail));
            }
        };
        (params_json, ssrc, None)
    };

    account.live().group_calls.joined = Some(JoinedGroupCall {
        who,
        call_id,
        access_hash,
        join_as: join_as_who,
        source: ssrc,
        muted: true,
        deafened: false,
        camera_on: false,
        presentation_active: false,
        applied_gains: HashMap::new(),
        pending_gains: HashMap::new(),
        rtmp: rtmp_viewer,
        next_check: Instant::now() + check_interval(),
        last_speaking_sent: None,
    });

    let clean_hash = invite_hash.map(|h| h.trim().to_owned()).filter(|h| !h.is_empty());
    let req = tl::functions::phone::JoinGroupCall {
        muted: true,
        video_stopped: true,
        call: input_call_of(call_id, access_hash),
        join_as: input_join_as,
        invite_hash: clean_hash,
        public_key: None,
        block: None,
        params: tl::types::DataJson { data: params_json }.into(),
    };
    match account.ask(&req) {
        Ok(answer) => {
            if !is_rtmp {
                live::answered(account, &answer);
            }
            // Fetch paged participants so the roster and SSRC map are complete.
            let _ = fetch_participants_pages(account, who, call_id, access_hash, MAX_PARTICIPANT_PAGES);
            publish_occupants(account, who);
            if is_rtmp {
                tick_rtmp(account);
            }
            Ok(())
        }
        Err(fault) => {
            account.live().group_calls.joined = None;
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            Err(fault)
        }
    }
}

/// Updates our own mute state in the joined group voice chat (`phone.editGroupCallParticipant`).
pub(super) fn set_group_audio(account: &Account, muted: bool, deafened: bool) {
    let target = {
        let mut live = account.live();
        let Some((who, join_as, call_id, access_hash)) = live
            .group_calls
            .joined
            .as_ref()
            .map(|j| (j.who, j.join_as, j.call_id, j.access_hash))
        else {
            return;
        };
        let suppressed = live
            .group_calls
            .calls
            .get(&who)
            .and_then(|c| c.participants.values().find(|p| p.is_self || p.who == join_as))
            .is_some_and(GroupParticipant::suppressed);
        // Never attempt to unmute while suppressed (listener without can_self_unmute).
        let eff_muted = if suppressed { true } else { muted };
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        let mute_changed = joined.muted != eff_muted;
        joined.muted = eff_muted;
        joined.deafened = deafened;
        mute_changed.then_some((call_id, access_hash, join_as, eff_muted))
    };
    if let Some((call_id, access_hash, join_as, eff_muted)) = target {
        let Ok(participant) = input_peer_for_identity(account, join_as) else {
            return;
        };
        let req = tl::functions::phone::EditGroupCallParticipant {
            call: input_call_of(call_id, access_hash),
            participant,
            muted: Some(eff_muted),
            volume: None,
            raise_hand: None,
            video_stopped: None,
            video_paused: None,
            presentation_paused: None,
        };
        if let Ok(answer) = account.ask(&req) {
            live::answered(account, &answer);
        }
    }
}

/// Sends `speakingInGroupCallAction` while speaking in a joined Telegram group voice chat.
pub(super) fn on_local_speaking(account: &Account, speaking: bool) {
    if !speaking {
        return;
    }
    let who = {
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        if joined.muted {
            return;
        }
        if joined.last_speaking_sent.is_some_and(|t| t.elapsed() < SPEAKING_PACE) {
            return;
        }
        joined.last_speaking_sent = Some(Instant::now());
        joined.who
    };
    if let Ok(peer) = account.input(who) {
        let _ = wire::call(
            account.wire.as_ref(),
            &tl::functions::messages::SetTyping {
                peer,
                top_msg_id: None,
                action: tl::enums::SendMessageAction::SpeakingInGroupCallAction,
            },
        );
    }
}

/// Leaves the joined group voice chat and always notifies Telegram (`phone.leaveGroupCall`).
pub(super) fn leave_group(account: &Account, who: Who) {
    let joined = {
        let mut live = account.live();
        if live.group_calls.joined.as_ref().is_some_and(|j| j.who == who) {
            live.group_calls.joined.take()
        } else {
            None
        }
    };
    let Some(joined) = joined else {
        return;
    };
    if joined.presentation_active {
        let _ = wire::call(
            account.wire.as_ref(),
            &tl::functions::phone::LeaveGroupCallPresentation {
                call: input_call_of(joined.call_id, joined.access_hash),
            },
        );
    }
    let req = tl::functions::phone::LeaveGroupCall {
        call: input_call_of(joined.call_id, joined.access_hash),
        source: joined.source as i32,
    };
    if let Ok(answer) = wire::call(account.wire.as_ref(), &req) {
        live::answered(account, &answer);
    }
}

/// Next deadline for `phone.checkGroupCall`, debounced participant gain flushes, or RTMP stream segment fetch.
pub(super) fn keepalive_deadline(account: &Account) -> Option<Instant> {
    let live = account.live();
    let j = live.group_calls.joined.as_ref()?;
    let gain_due = j.pending_gains.values().map(|g| g.due).min();
    let rtmp_due = j
        .rtmp
        .as_ref()
        .filter(|r| r.state != RtmpStreamState::Ended)
        .map(|r| r.next_fetch);
    [Some(j.next_check), gain_due, rtmp_due].into_iter().flatten().min()
}

/// Upper bound on event-loop sleep so keepalives, debounced gain flushes, and RTMP polls fire promptly.
pub(super) fn poll_cap() -> Duration {
    check_interval()
        .min(gain_debounce().max(Duration::from_millis(25)))
        .min(Duration::from_millis(250))
}

/// Drives debounced participant gain flushes (#468), RTMP segment polling (#454), and
/// `phone.checkGroupCall` keepalives (#461) on the event loop.
pub(super) fn tick_keepalive(account: &Account) {
    flush_pending_gains(account);
    tick_rtmp(account);
    let due = {
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        if Instant::now() < joined.next_check {
            return;
        }
        joined.next_check = Instant::now() + check_interval();
        (joined.who, joined.call_id, joined.access_hash, joined.source)
    };
    let (who, call_id, access_hash, source) = due;
    let req = tl::functions::phone::CheckGroupCall {
        call: input_call_of(call_id, access_hash),
        sources: vec![source as i32],
    };
    match wire::call(account.wire.as_ref(), &req) {
        Ok(missing) if missing.contains(&(source as i32)) => {
            tracing::debug!("chatd: Telegram: checkGroupCall reported missing source {source}");
            let _ = fetch_participants_pages(account, who, call_id, access_hash, 1);
        }
        Err(fault) if fault.is("GROUPCALL_INVALID") || fault.is("GROUPCALL_JOIN_MISSING") => {
            clear_discarded(account, who, call_id);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(who: Who, muted: bool, can_self_unmute: bool, speaking: bool, raise_hand_rating: Option<i64>, order: u64) -> GroupParticipant {
        GroupParticipant {
            who,
            source: 1,
            muted,
            can_self_unmute,
            speaking,
            raise_hand_rating,
            about: String::new(),
            muted_by_you: false,
            volume: None,
            video: None,
            presentation: None,
            is_self: false,
            order,
        }
    }

    #[test]
    fn sort_participants_puts_speakers_then_raised_hands_then_listeners() {
        let items = [
            mk(Who::User(4), true, false, false, None, 0),          // listener
            mk(Who::User(3), true, false, false, Some(20), 1),      // raised hand low
            mk(Who::User(2), true, false, false, Some(500), 2),     // raised hand high
            mk(Who::Channel(9), true, true, false, None, 3),        // muted speaker (channel)
            mk(Who::User(1), false, true, true, None, 4),           // active speaker
        ];
        let ordered: Vec<Who> = sort_participants(items.iter()).into_iter().map(|p| p.who).collect();
        assert_eq!(
            ordered,
            vec![Who::User(1), Who::Channel(9), Who::User(2), Who::User(3), Who::User(4)]
        );
    }
}
