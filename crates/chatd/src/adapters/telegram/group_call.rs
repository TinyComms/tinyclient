//! Telegram group and channel video chats (`#462` live presence, `#465` scheduled video chats,
//! `#461` joining group voice chats).
//!
//! - Reads `chatFull.call` / `channelFull.call` and `phone.getGroupCall` once when an active group
//!   or channel is opened (never polling non-open chats).
//! - Applies `updateGroupCall`, `updateGroupCallParticipants`, `updateGroupCallConnection`, and
//!   `speakingInGroupCallAction`.
//! - Schedules (`phone.createGroupCall` with `schedule_date`), starts (`phone.startScheduledGroupCall`),
//!   and toggles start reminders (`phone.toggleGroupCallStartSubscription`).
//! - Joins live group voice chats muted by default (`phone.joinGroupCall`), pages participants
//!   (`phone.getGroupParticipants`), runs the `phone.checkGroupCall` keepalive timer, forwards
//!   mute/unmute (`phone.editGroupCallParticipant`), sends `speakingInGroupCallAction`, and always
//!   sends `phone.leaveGroupCall` on leave.

use super::peers::Who;
use super::wire::{self, Fault};
use super::{live, sync, Account};
use grammers_tl_types as tl;
use protocol::{CallFailure, Occupant, Occupants, Participant, TelegramGroupOffer, TelegramGroupSource};
use std::collections::HashMap;
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

/// Minimum spacing between outgoing `speakingInGroupCallAction` typing updates.
const SPEAKING_PACE: Duration = Duration::from_secs(4);
/// Maximum pages fetched from `phone.getGroupParticipants` on join.
const MAX_PARTICIPANT_PAGES: usize = 5;
/// Participants requested per page.
const PARTICIPANT_PAGE_SIZE: i32 = 50;

#[derive(Clone, Debug)]
pub(super) struct GroupParticipant {
    pub who: Who,
    pub source: u32,
    pub muted: bool,
    pub can_self_unmute: bool,
    pub speaking: bool,
    pub order: u64,
}

#[derive(Clone, Debug)]
pub(super) struct TrackedGroupCall {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
    pub participants_count: u32,
    pub schedule_date: Option<i64>,
    pub schedule_start_subscribed: bool,
    pub participants: HashMap<Who, GroupParticipant>,
    pub next_order: u64,
    pub fetched_once: bool,
}

#[derive(Clone, Debug)]
pub(super) struct JoinedGroupCall {
    pub who: Who,
    pub call_id: i64,
    pub access_hash: i64,
    pub source: u32,
    pub muted: bool,
    pub deafened: bool,
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
        .or_insert_with(|| TrackedGroupCall {
            id,
            access_hash,
            title: String::new(),
            participants_count: 0,
            schedule_date: None,
            schedule_start_subscribed: false,
            participants: HashMap::new(),
            next_order: 0,
            fetched_once: false,
        });
}

/// Fetches `InputGroupCall` for `who` via `GetFullChat` / `GetFullChannel` if not yet cached.
fn ensure_input_call(account: &Account, who: Who) -> Result<Option<(i64, i64)>, Fault> {
    if let Some((id, hash)) = account.live().group_calls.calls.get(&who).map(|c| (c.id, c.access_hash)) {
        return Ok(Some((id, hash)));
    }
    match who {
        Who::Group(chat_id) => {
            let tl::enums::messages::ChatFull::Full(full) =
                account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
            account.learn(&full.users, &full.chats);
            if let tl::enums::ChatFull::Full(chat) = full.full_chat {
                note_full_call(account, who, chat.call.as_ref());
                return Ok(chat.call.as_ref().and_then(extract_input_call));
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let tl::enums::messages::ChatFull::Full(full) =
                account.ask(&tl::functions::channels::GetFullChannel { channel })?;
            account.learn(&full.users, &full.chats);
            if let tl::enums::ChatFull::ChannelFull(channel) = full.full_chat {
                note_full_call(account, who, channel.call.as_ref());
                return Ok(channel.call.as_ref().and_then(extract_input_call));
            }
        }
        Who::User(_) => {}
    }
    Ok(None)
}

fn apply_group_call_obj(tracked: &mut TrackedGroupCall, gc: &tl::types::GroupCall) {
    tracked.id = gc.id;
    tracked.access_hash = gc.access_hash;
    tracked.title = gc.title.clone().unwrap_or_default();
    tracked.participants_count = gc.participants_count.max(0) as u32;
    tracked.schedule_date = gc.schedule_date.filter(|&d| d > 0).map(i64::from);
    tracked.schedule_start_subscribed = gc.schedule_start_subscribed;
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
        let order = tracked.participants.get(&who).map(|old| old.order).unwrap_or_else(|| {
            let next = tracked.next_order;
            tracked.next_order += 1;
            next
        });
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

fn publish_occupants(account: &Account, who: Who) {
    let (occupants, joined_match, call_participants, sources) = {
        let live = account.live();
        let Some(tracked) = live.group_calls.calls.get(&who) else {
            drop(live);
            account.ctx.group_call_occupants(&who.text(), Occupants::default());
            sync::refresh_row(account, who);
            return;
        };
        let dir = account.dir();
        let mut sorted: Vec<&GroupParticipant> = tracked.participants.values().collect();
        sorted.sort_by_key(|p| (!p.speaking, p.muted, p.order));
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
                    suppressed: !p.can_self_unmute && p.muted,
                    speaking: p.speaking,
                    streaming: false,
                    camera: false,
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
        };

        let joined_here = live.group_calls.joined.as_ref().is_some_and(|j| j.who == who);
        let (call_participants, sources) = if joined_here {
            let me = Who::User(account.me);
            let participants: Vec<Participant> = sorted
                .iter()
                .filter(|p| p.who != me)
                .map(|p| {
                    let id = p.who.text();
                    Participant {
                        id,
                        name: dir.name(p.who),
                        muted: p.muted,
                        deafened: false,
                        speaking: p.speaking,
                        suppressed: !p.can_self_unmute && p.muted,
                        ..Participant::default()
                    }
                })
                .collect();
            let sources: Vec<TelegramGroupSource> = sorted
                .iter()
                .filter(|p| p.who != me && p.source > 0)
                .map(|p| TelegramGroupSource {
                    ssrc: p.source,
                    user_id: p.who.text(),
                })
                .collect();
            (participants, sources)
        } else {
            (Vec::new(), Vec::new())
        };
        (occupants, joined_here, call_participants, sources)
    };

    let conv_id = who.text();
    account.ctx.group_call_occupants(&conv_id, occupants);
    sync::refresh_row(account, who);
    if joined_match {
        account.ctx.participants(&conv_id, call_participants);
        account.ctx.telegram_group_sources(&conv_id, sources);
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
                let tracked = live.group_calls.calls.entry(who).or_insert_with(|| TrackedGroupCall {
                    id: gc.id,
                    access_hash: gc.access_hash,
                    title: String::new(),
                    participants_count: 0,
                    schedule_date: None,
                    schedule_start_subscribed: false,
                    participants: HashMap::new(),
                    next_order: 0,
                    fetched_once: false,
                });
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
            let need_participants = {
                let mut live = account.live();
                live.group_calls.by_call_id.insert(gc.id, who);
                let prev_scheduled = live.group_calls.calls.get(&who).and_then(|c| c.schedule_date);
                let tracked = live.group_calls.calls.entry(who).or_insert_with(|| TrackedGroupCall {
                    id: gc.id,
                    access_hash: gc.access_hash,
                    title: String::new(),
                    participants_count: 0,
                    schedule_date: None,
                    schedule_start_subscribed: false,
                    participants: HashMap::new(),
                    next_order: 0,
                    fetched_once: false,
                });
                apply_group_call_obj(tracked, gc);
                let just_went_live = prev_scheduled.is_some() && tracked.schedule_date.is_none();
                (just_went_live || !tracked.fetched_once) && is_chat_watched(account, who)
            };
            account.dir().set_call_active(who, true, gc.participants_count > 0);
            publish_occupants(account, who);
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

/// Handles `Update::GroupCallConnection` (`#461`).
pub(super) fn on_connection(account: &Account, update: &tl::types::UpdateGroupCallConnection) {
    if update.presentation {
        return;
    }
    let tl::enums::DataJson::Json(json) = &update.params;
    let conv_id = account.live().group_calls.joined.as_ref().map(|j| j.who.text());
    if let Some(conv_id) = conv_id {
        account.ctx.telegram_group_answer(&conv_id, json.data.clone());
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
// Join, Audio Mute, Speaking, Keepalive, Leave (`#461`)
// ---------------------------------------------------------------------------

/// Joins a live Telegram group or channel voice chat (`Command::CallJoin` on `g<id>` or `c<id>`).
pub(super) fn join_group(account: &Account, who: Who) {
    let conv_id = who.text();
    let call_ref = match ensure_input_call(account, who) {
        Ok(Some(c)) => c,
        Ok(None) => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, "There is no live video chat in this chat.");
            return;
        }
        Err(fault) => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            return;
        }
    };
    // Ensure we know whether the call is live (not scheduled-only).
    if account.live().group_calls.calls.get(&who).is_some_and(|c| c.schedule_date.is_some()) {
        account.ctx.call_failed(&conv_id, CallFailure::Refused, "This video chat hasn't started yet.");
        return;
    }

    let me_id = Who::User(account.me).text();
    // Always join muted by default (`#461`).
    let (params_json, ssrc, _pk) = match account.ctx.telegram_group_offer(
        &conv_id,
        TelegramGroupOffer {
            user_id: me_id,
            muted: true,
            deafened: false,
            conference: false,
        },
    ) {
        Ok(triple) => triple,
        Err(detail) => {
            account.ctx.call_failed(&conv_id, CallFailure::Engine, &detail);
            return;
        }
    };

    let (call_id, access_hash) = call_ref;
    account.live().group_calls.joined = Some(JoinedGroupCall {
        who,
        call_id,
        access_hash,
        source: ssrc,
        muted: true,
        deafened: false,
        next_check: Instant::now() + check_interval(),
        last_speaking_sent: None,
    });

    let req = tl::functions::phone::JoinGroupCall {
        muted: true,
        video_stopped: true,
        call: input_call_of(call_id, access_hash),
        join_as: tl::enums::InputPeer::PeerSelf,
        invite_hash: None,
        public_key: None,
        block: None,
        params: tl::types::DataJson { data: params_json }.into(),
    };
    match account.ask(&req) {
        Ok(answer) => {
            live::answered(account, &answer);
            // Fetch paged participants so the roster and SSRC map are complete.
            let _ = fetch_participants_pages(account, who, call_id, access_hash, MAX_PARTICIPANT_PAGES);
        }
        Err(fault) => {
            account.live().group_calls.joined = None;
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
        }
    }
}

/// Updates our own mute state in the joined group voice chat (`phone.editGroupCallParticipant`).
pub(super) fn set_group_audio(account: &Account, muted: bool, deafened: bool) {
    let target = {
        let mut live = account.live();
        let Some(joined) = live.group_calls.joined.as_mut() else {
            return;
        };
        let mute_changed = joined.muted != muted;
        joined.muted = muted;
        joined.deafened = deafened;
        mute_changed.then_some((joined.call_id, joined.access_hash))
    };
    if let Some((call_id, access_hash)) = target {
        let req = tl::functions::phone::EditGroupCallParticipant {
            call: input_call_of(call_id, access_hash),
            participant: tl::enums::InputPeer::PeerSelf,
            muted: Some(muted),
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
    let req = tl::functions::phone::LeaveGroupCall {
        call: input_call_of(joined.call_id, joined.access_hash),
        source: joined.source as i32,
    };
    if let Ok(answer) = wire::call(account.wire.as_ref(), &req) {
        live::answered(account, &answer);
    }
}

/// Next deadline for `phone.checkGroupCall` keepalive.
pub(super) fn keepalive_deadline(account: &Account) -> Option<Instant> {
    account.live().group_calls.joined.as_ref().map(|j| j.next_check)
}

/// Upper bound on event-loop sleep so a newly joined group call starts its keepalive promptly.
pub(super) fn poll_cap() -> Duration {
    check_interval().min(Duration::from_secs(5))
}

/// Sends `phone.checkGroupCall` when due on the event loop (`#461`).
pub(super) fn tick_keepalive(account: &Account) {
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
