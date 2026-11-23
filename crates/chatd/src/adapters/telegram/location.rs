//! Telegram one-time and live location sharing (#452):
//! - One-time location via `inputMediaGeoPoint`.
//! - Live location start via `inputMediaGeoLive` (15m / 1h / 8h / until turned off, plus optional
//!   proximity alert radius in direct chats).
//! - Rate-limited live coordinate edits via `messages.editMessage` (`inputMediaGeoLive`).
//! - Explicit stop (`stopped: true`), stop-all, automatic stop on expiry/sign-out/session loss,
//!   and restart/reconnect recovery of still-active live shares.

use super::peers::Who;
use super::wire::Fault;
use super::{live, random_id, remember_random, reply_header, Account};
use crate::hub::now;
use grammers_tl_types as tl;
use protocol::{GeoPoint, LiveLocationShare, Message, LIVE_LOCATION_INDEFINITE};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Persistent note key under which active live location shares are saved across restarts.
const PERSIST_NOTE: &str = "tg-live-geo";
/// Production minimum interval between `messages.editMessage` calls for one live share.
const PROD_EDIT_PACE: Duration = Duration::from_secs(5);
/// Faster pace under the scripted test wire so rate-limiting and sequential edits stay fast.
const SCRIPTED_EDIT_PACE: Duration = Duration::from_millis(50);

fn edit_pace() -> Duration {
    if super::scripted() {
        SCRIPTED_EDIT_PACE
    } else {
        PROD_EDIT_PACE
    }
}

/// Per-connection live-location tracking state inside `Account::live`.
#[derive(Default)]
pub(super) struct State {
    /// Active live shares keyed by `(conversation_id, message_id)`.
    shares: HashMap<(String, String), TrackedShare>,
}

#[derive(Clone)]
struct TrackedShare {
    share: LiveLocationShare,
    last_edit_at: Option<Instant>,
    pending_edit: Option<(GeoPoint, Option<u32>)>,
}

fn input_geo_point(point: GeoPoint) -> tl::enums::InputGeoPoint {
    tl::types::InputGeoPoint {
        lat: point.lat(),
        long: point.lon(),
        accuracy_radius: point.accuracy_meters.map(|m| m as i32),
    }
    .into()
}

fn build_send_media(
    peer: tl::enums::InputPeer,
    reply_to: Option<tl::enums::InputReplyTo>,
    media: tl::enums::InputMedia,
    random_id: i64,
) -> tl::functions::messages::SendMedia {
    tl::functions::messages::SendMedia {
        silent: false,
        background: false,
        clear_draft: false,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer,
        reply_to,
        media,
        message: String::new(),
        random_id,
        reply_markup: None,
        entities: None,
        schedule_date: None,
        schedule_repeat_period: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        allow_paid_stars: None,
        suggested_post: None,
    }
}

fn build_edit_geo_live(
    peer: tl::enums::InputPeer,
    id: i32,
    media: tl::types::InputMediaGeoLive,
) -> tl::functions::messages::EditMessage {
    tl::functions::messages::EditMessage {
        no_webpage: false,
        invert_media: false,
        peer,
        id,
        message: None,
        media: Some(media.into()),
        reply_markup: None,
        entities: None,
        schedule_date: None,
        schedule_repeat_period: None,
        quick_reply_shortcut_id: None,
        rich_message: None,
    }
}

fn conversation_title(account: &Account, conversation: &str, who: Who) -> String {
    account
        .ctx
        .stored_conversation(conversation)
        .ok()
        .flatten()
        .map(|row| row.title)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| account.dir().name(who))
}

fn sorted_shares(state: &State) -> Vec<LiveLocationShare> {
    let mut list: Vec<LiveLocationShare> = state.shares.values().map(|t| t.share.clone()).collect();
    list.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.conversation.id.cmp(&b.conversation.id))
            .then_with(|| a.message_id.cmp(&b.message_id))
    });
    list
}

fn persist_and_publish(account: &Account) {
    let shares = {
        let live = account.live();
        sorted_shares(&live.live_geo)
    };
    if shares.is_empty() {
        account.ctx.set_note(PERSIST_NOTE, "");
    } else if let Ok(json) = serde_json::to_string(&shares) {
        account.ctx.set_note(PERSIST_NOTE, &json);
    }
    account.ctx.live_locations(shares);
}

/// Sends a one-time location (`inputMediaGeoPoint`) to `conversation`.
pub(super) fn send_one_time(
    account: &Account,
    conversation: &str,
    client_id: &str,
    point: GeoPoint,
    reply_to: Option<&str>,
) -> Result<Message, Fault> {
    if super::secret::is_row(conversation) {
        return Err(Fault::Rpc {
            code: 400,
            name: "SECRET_CHAT".to_owned(),
            value: None,
        });
    }
    let (who, topic) = Who::parse(conversation)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let (who, topic) = super::sync::comment_target(account, conversation)?
        .map(|(group, echo)| (group, Some(echo)))
        .unwrap_or((who, topic));
    let peer = account.input(who)?;
    let reply = reply_to.and_then(|id| id.parse::<i32>().ok());
    let random = random_id(client_id);
    account.here();
    remember_random(account, random, client_id);
    let media: tl::enums::InputMedia = tl::types::InputMediaGeoPoint {
        geo_point: input_geo_point(point),
    }
    .into();
    let answer = account.ask(&build_send_media(peer, reply_header(reply, topic), media, random))?;
    let message = live::sent(account, conversation, who, "", reply, &answer)
        .ok_or_else(|| Fault::Net("Telegram accepted the location but did not name it".to_owned()))?;
    let _ = account.ctx.message(conversation, &message);
    Ok(message)
}

/// Starts a live location share (`inputMediaGeoLive`) with the user-chosen `period_secs` and
/// optional proximity alert radius (direct chats only).
pub(super) fn start_live(
    account: &Account,
    conversation: &str,
    client_id: &str,
    point: GeoPoint,
    period_secs: u32,
    proximity_alert_radius: Option<u32>,
    reply_to: Option<&str>,
) -> Result<LiveLocationShare, Fault> {
    if super::secret::is_row(conversation) {
        return Err(Fault::Rpc {
            code: 400,
            name: "SECRET_CHAT".to_owned(),
            value: None,
        });
    }
    let (who, topic) = Who::parse(conversation)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let (who, topic) = super::sync::comment_target(account, conversation)?
        .map(|(group, echo)| (group, Some(echo)))
        .unwrap_or((who, topic));
    let peer = account.input(who)?;
    let reply = reply_to.and_then(|id| id.parse::<i32>().ok());
    let random = random_id(client_id);
    // Proximity alert radius is supported only in direct chats.
    let proximity = if matches!(who, Who::User(_)) {
        proximity_alert_radius.filter(|&r| r > 0)
    } else {
        None
    };
    account.here();
    remember_random(account, random, client_id);
    let media: tl::enums::InputMedia = tl::types::InputMediaGeoLive {
        stopped: false,
        geo_point: input_geo_point(point),
        heading: point.heading.map(i32::from),
        period: Some(period_secs as i32),
        proximity_notification_radius: proximity.map(|r| r as i32),
    }
    .into();
    let answer = account.ask(&build_send_media(peer, reply_header(reply, topic), media, random))?;
    let message = live::sent(account, conversation, who, "", reply, &answer)
        .ok_or_else(|| Fault::Net("Telegram accepted the live location but did not name it".to_owned()))?;
    let _ = account.ctx.message(conversation, &message);

    let started_at = if message.sent > 0 { message.sent } else { now() };
    let expires_at = if period_secs == LIVE_LOCATION_INDEFINITE {
        None
    } else {
        Some(started_at + i64::from(period_secs))
    };
    let share = LiveLocationShare {
        conversation: account.ctx.key(conversation),
        conversation_title: conversation_title(account, conversation, who),
        message_id: message.id.clone(),
        point,
        started_at,
        period_secs,
        expires_at,
        proximity_alert_radius: proximity,
        last_updated_at: now(),
    };
    {
        let mut live = account.live();
        live.live_geo.shares.insert(
            (conversation.to_owned(), message.id),
            TrackedShare {
                share: share.clone(),
                last_edit_at: Some(Instant::now()),
                pending_edit: None,
            },
        );
    }
    persist_and_publish(account);
    Ok(share)
}

/// Updates coordinates of an active live location share via `messages.editMessage`
/// (`inputMediaGeoLive`). Edits arriving inside the rate-limit window are coalesced and flushed
/// automatically by [`tick`] once the pace elapses.
pub(super) fn update_live(
    account: &Account,
    conversation: &str,
    message_id: &str,
    point: GeoPoint,
    proximity_alert_radius: Option<u32>,
) -> Result<LiveLocationShare, Fault> {
    let (who, _) = Who::parse(conversation)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let msg_id: i32 = message_id
        .parse()
        .map_err(|_| Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None })?;
    let key = (conversation.to_owned(), message_id.to_owned());
    let proximity = if matches!(who, Who::User(_)) {
        proximity_alert_radius.filter(|&r| r > 0)
    } else {
        None
    };

    let now_s = now();
    let pace = edit_pace();
    let (send_now, updated_share) = {
        let mut live = account.live();
        let entry = live.live_geo.shares.get_mut(&key).ok_or_else(|| Fault::Rpc {
            code: 400,
            name: "MESSAGE_ID_INVALID".to_owned(),
            value: None,
        })?;
        if !entry.share.is_active_at(now_s) {
            return Err(Fault::Rpc {
                code: 400,
                name: "MESSAGE_EDIT_TIME_EXPIRED".to_owned(),
                value: None,
            });
        }
        entry.share.point = point;
        entry.share.last_updated_at = now_s;
        if proximity.is_some() {
            entry.share.proximity_alert_radius = proximity;
        }
        let within_pace = entry
            .last_edit_at
            .is_some_and(|at| at.elapsed() < pace);
        if within_pace {
            entry.pending_edit = Some((point, entry.share.proximity_alert_radius));
            (false, entry.share.clone())
        } else {
            entry.pending_edit = None;
            entry.last_edit_at = Some(Instant::now());
            (true, entry.share.clone())
        }
    };

    if send_now {
        let peer = account.input(who)?;
        let media = tl::types::InputMediaGeoLive {
            stopped: false,
            geo_point: input_geo_point(point),
            heading: point.heading.map(i32::from),
            period: None,
            proximity_notification_radius: updated_share.proximity_alert_radius.map(|r| r as i32),
        };
        let answer = account.ask(&build_edit_geo_live(peer, msg_id, media))?;
        live::answered(account, &answer);
    }

    persist_and_publish(account);
    Ok(updated_share)
}

/// Stops one active live location share (`messages.editMessage` with `stopped: true`).
pub(super) fn stop_live(account: &Account, conversation: &str, message_id: &str) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let msg_id: i32 = message_id
        .parse()
        .map_err(|_| Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None })?;
    let key = (conversation.to_owned(), message_id.to_owned());
    let last_point = account
        .live()
        .live_geo
        .shares
        .get(&key)
        .map(|t| t.share.point);
    let peer = account.input(who)?;
    account.here();
    let geo_point = last_point
        .map(input_geo_point)
        .unwrap_or(tl::enums::InputGeoPoint::Empty);
    let media = tl::types::InputMediaGeoLive {
        stopped: true,
        geo_point,
        heading: None,
        period: None,
        proximity_notification_radius: None,
    };
    let result = account.ask(&build_edit_geo_live(peer, msg_id, media));
    match result {
        Ok(answer) => {
            live::answered(account, &answer);
        }
        Err(fault) if fault.is("MESSAGE_NOT_MODIFIED") || fault.is("MESSAGE_EDIT_TIME_EXPIRED") || fault.is("MESSAGE_ID_INVALID") => {
            // Already stopped or expired on Telegram — clean up local state cleanly.
        }
        Err(fault) => return Err(fault),
    }
    account.live().live_geo.shares.remove(&key);
    persist_and_publish(account);
    Ok(())
}

/// Stops every active live location share on this account.
pub(super) fn stop_all(account: &Account) -> Result<(), Fault> {
    let keys: Vec<(String, String)> = account
        .live()
        .live_geo
        .shares
        .keys()
        .cloned()
        .collect();
    let mut first_err = None;
    for (conv, msg_id) in keys {
        if let Err(fault) = stop_live(account, &conv, &msg_id) {
            if first_err.is_none() {
                first_err = Some(fault);
            }
        }
    }
    if first_err.is_none() {
        account.live().live_geo.shares.clear();
        persist_and_publish(account);
    }
    match first_err {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Lists active live shares (pruning any expired shares first).
pub(super) fn active_list(account: &Account) -> Vec<LiveLocationShare> {
    let now_s = now();
    let pruned = {
        let mut live = account.live();
        let before = live.live_geo.shares.len();
        live.live_geo.shares.retain(|_, t| t.share.is_active_at(now_s));
        live.live_geo.shares.len() != before
    };
    if pruned {
        persist_and_publish(account);
    }
    sorted_shares(&account.live().live_geo)
}

/// Returns the earliest instant at which [`tick`] has work (either an expiry or a rate-limited
/// coordinate flush).
pub(super) fn next_deadline(account: &Account) -> Option<Instant> {
    let now_s = now();
    let pace = edit_pace();
    let now_inst = Instant::now();
    let live = account.live();
    let mut soonest: Option<Instant> = None;
    for tracked in live.live_geo.shares.values() {
        if let Some(until) = tracked.share.expires_at {
            let secs = (until - now_s).max(0) as u64;
            let deadline = now_inst + Duration::from_secs(secs);
            soonest = Some(soonest.map_or(deadline, |s| s.min(deadline)));
        }
        if tracked.pending_edit.is_some() {
            let deadline = tracked
                .last_edit_at
                .map_or(now_inst, |at| at + pace);
            soonest = Some(soonest.map_or(deadline, |s| s.min(deadline)));
        }
    }
    soonest
}

/// Runs on the connection's event loop: automatically stops expired shares and flushes any
/// coalesced coordinate edits whose rate-limit window has elapsed.
pub(super) fn tick(account: &Account) {
    let now_s = now();
    let pace = edit_pace();
    let mut expired = Vec::new();
    let mut to_flush = Vec::new();
    {
        let mut live = account.live();
        for ((conv, msg_id), tracked) in &mut live.live_geo.shares {
            if !tracked.share.is_active_at(now_s) {
                expired.push((conv.clone(), msg_id.clone()));
                continue;
            }
            let ready = tracked
                .last_edit_at
                .is_none_or(|at| at.elapsed() >= pace);
            if ready {
                if let Some((point, proximity)) = tracked.pending_edit.take() {
                    tracked.last_edit_at = Some(Instant::now());
                    to_flush.push((conv.clone(), msg_id.clone(), point, proximity));
                }
            }
        }
    }

    for (conv, msg_id) in expired {
        let _ = stop_live(account, &conv, &msg_id);
    }

    for (conv, msg_id, point, proximity) in to_flush {
        if let (Some((who, _)), Ok(id)) = (Who::parse(&conv), msg_id.parse::<i32>()) {
            if let Ok(peer) = account.input(who) {
                let media = tl::types::InputMediaGeoLive {
                    stopped: false,
                    geo_point: input_geo_point(point),
                    heading: point.heading.map(i32::from),
                    period: None,
                    proximity_notification_radius: proximity.map(|r| r as i32),
                };
                if let Ok(answer) = account.ask(&build_edit_geo_live(peer, id, media)) {
                    live::answered(account, &answer);
                }
            }
        }
    }
}

/// Observes an outgoing `MessageMedia::GeoLive` message from dialogs or live updates so active
/// live shares started on another device (or stopped from another device) stay accurate.
pub(super) fn observe_message(account: &Account, conversation: &str, raw: &tl::types::Message) {
    if !raw.out {
        return;
    }
    let Some(tl::enums::MessageMedia::GeoLive(geo_live)) = &raw.media else {
        return;
    };
    let key = (conversation.to_owned(), raw.id.to_string());
    let now_s = now();
    let is_indef = geo_live.period == LIVE_LOCATION_INDEFINITE as i32;
    let active = is_indef
        || (geo_live.period > 0 && i64::from(raw.date) + i64::from(geo_live.period) > now_s);
    if !active {
        let removed = account.live().live_geo.shares.remove(&key).is_some();
        if removed {
            persist_and_publish(account);
        }
        return;
    }
    let Some(point) = geo_point_from_tl(&geo_live.geo, geo_live.heading) else {
        return;
    };
    let Some((who, _)) = Who::parse(conversation) else {
        return;
    };
    let period_secs = geo_live.period as u32;
    let started_at = i64::from(raw.date);
    let expires_at = if is_indef {
        None
    } else {
        Some(started_at + i64::from(geo_live.period))
    };
    let share = LiveLocationShare {
        conversation: account.ctx.key(conversation),
        conversation_title: conversation_title(account, conversation, who),
        message_id: raw.id.to_string(),
        point,
        started_at,
        period_secs,
        expires_at,
        proximity_alert_radius: geo_live
            .proximity_notification_radius
            .filter(|&r| r > 0)
            .map(|r| r as u32),
        last_updated_at: raw.edit_date.map(i64::from).unwrap_or(started_at),
    };
    let changed = {
        let mut live = account.live();
        match live.live_geo.shares.get_mut(&key) {
            Some(existing) => {
                if existing.share != share {
                    existing.share = share;
                    true
                } else {
                    false
                }
            }
            None => {
                live.live_geo.shares.insert(
                    key,
                    TrackedShare {
                        share,
                        last_edit_at: None,
                        pending_edit: None,
                    },
                );
                true
            }
        }
    };
    if changed {
        persist_and_publish(account);
    }
}

fn geo_point_from_tl(geo: &tl::enums::GeoPoint, heading: Option<i32>) -> Option<GeoPoint> {
    let tl::enums::GeoPoint::Point(pt) = geo else {
        return None;
    };
    let mut point = GeoPoint::from_deg(pt.lat, pt.long).ok()?;
    point = point
        .with_accuracy(pt.accuracy_radius.filter(|&m| m > 0).map(|m| m as u32))
        .with_heading(heading.filter(|&h| (1..=360).contains(&h)).map(|h| h as u16));
    Some(point)
}

/// On sign-on / reconnect, verifies persisted live shares against Telegram (`messages.getMessages`
/// / `channels.getMessages`) so only truly active live shares remain in the sharing bar.
pub(super) fn recover_on_reconnect(account: &Account) {
    let saved: Vec<LiveLocationShare> = account
        .ctx
        .note(PERSIST_NOTE)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();

    let now_s = now();
    let mut recovered: HashMap<(String, String), TrackedShare> = HashMap::new();

    // Keep anything already observed from the initial dialogs fetch that is still active.
    {
        let live = account.live();
        for (k, v) in &live.live_geo.shares {
            if v.share.is_active_at(now_s) {
                recovered.insert(k.clone(), v.clone());
            }
        }
    }

    for item in saved {
        let conv = item.conversation.id.clone();
        let key = (conv.clone(), item.message_id.clone());
        let Ok(msg_id) = item.message_id.parse::<i32>() else {
            continue;
        };
        let Some((who, _)) = Who::parse(&conv) else {
            continue;
        };
        // Ask Telegram for the current state of that message to verify it is still live.
        let fetched = fetch_message(account, who, msg_id);
        match fetched {
            Ok(Some(tl::enums::Message::Message(raw))) if raw.out => {
                if let Some(tl::enums::MessageMedia::GeoLive(gl)) = &raw.media {
                    let is_indef = gl.period == LIVE_LOCATION_INDEFINITE as i32;
                    let still_live = is_indef
                        || (gl.period > 0 && i64::from(raw.date) + i64::from(gl.period) > now_s);
                    if still_live {
                        let point = geo_point_from_tl(&gl.geo, gl.heading).unwrap_or(item.point);
                        let started_at = i64::from(raw.date);
                        let expires_at = if is_indef {
                            None
                        } else {
                            Some(started_at + i64::from(gl.period))
                        };
                        let share = LiveLocationShare {
                            conversation: account.ctx.key(&conv),
                            conversation_title: conversation_title(account, &conv, who),
                            message_id: item.message_id,
                            point,
                            started_at,
                            period_secs: gl.period as u32,
                            expires_at,
                            proximity_alert_radius: gl
                                .proximity_notification_radius
                                .filter(|&r| r > 0)
                                .map(|r| r as u32),
                            last_updated_at: raw.edit_date.map(i64::from).unwrap_or(started_at),
                        };
                        recovered.insert(
                            key,
                            TrackedShare {
                                share,
                                last_edit_at: None,
                                pending_edit: None,
                            },
                        );
                    }
                }
            }
            // Deleted, replaced, stopped, or not ours: do not restore.
            Ok(_) => {}
            // Transient network error while verifying: keep only if not yet expired locally.
            Err(_) => {
                if item.is_active_at(now_s) {
                    recovered.entry(key).or_insert(TrackedShare {
                        share: item,
                        last_edit_at: None,
                        pending_edit: None,
                    });
                }
            }
        }
    }

    {
        let mut live = account.live();
        live.live_geo.shares = recovered;
    }
    persist_and_publish(account);
}

fn fetch_message(account: &Account, who: Who, msg_id: i32) -> Result<Option<tl::enums::Message>, Fault> {
    let ids = vec![tl::types::InputMessageId { id: msg_id }.into()];
    let resp = match who {
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            account.ask(&tl::functions::channels::GetMessages { channel, id: ids })?
        }
        _ => account.ask(&tl::functions::messages::GetMessages { id: ids })?,
    };
    let messages = match resp {
        tl::enums::messages::Messages::Messages(m) => m.messages,
        tl::enums::messages::Messages::Slice(m) => m.messages,
        tl::enums::messages::Messages::ChannelMessages(m) => m.messages,
        tl::enums::messages::Messages::NotModified(_) => Vec::new(),
    };
    Ok(messages.into_iter().find(|m| m.id() == msg_id))
}

/// Handles sign-out or session loss: clears active shares, clears persisted state on sign-out,
/// and notifies the shell so the active-sharing bar never lies after sign-out.
pub(super) fn on_session_lost(account: &Account, signed_out: bool) {
    if signed_out {
        // Session is revoked or user signed out: clear persisted state and active shares.
        account.live().live_geo.shares.clear();
        account.ctx.set_note(PERSIST_NOTE, "");
        account.ctx.live_locations(Vec::new());
    } else if account.ctx.stopped() {
        // Explicit adapter stop (e.g. sign-out before wire closes): best-effort stop_all.
        let _ = stop_all(account);
        account.live().live_geo.shares.clear();
        account.ctx.set_note(PERSIST_NOTE, "");
        account.ctx.live_locations(Vec::new());
    } else {
        // Transient disconnect: drop pending timers; persisted note remains for reconnect recovery.
        for tracked in account.live().live_geo.shares.values_mut() {
            tracked.pending_edit = None;
        }
    }
}
