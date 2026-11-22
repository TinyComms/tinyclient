// A group call on the live socket. The shell's conversation is the group id. Media is
// `g:{call_id}`, and the wire is `call.group.*` plus a `call.join` that names `group_id`.
// A voice room's `call.joined` never sees a `g:` room: those frames stay here.

/// One group call this socket knows about.
#[derive(Clone)]
struct GroupCall {
    call_id: String,
    starter: String,
    people: Vec<Sitting>,
    /// `(user id, state word)` as the server last said it.
    rings: Vec<(String, String)>,
    speaking: Vec<String>,
    seated: bool,
    joining: bool,
}

impl GroupCall {
    fn new(call_id: &str, starter: &str) -> GroupCall {
        GroupCall {
            call_id: call_id.to_owned(),
            starter: starter.to_owned(),
            people: Vec::new(),
            rings: Vec::new(),
            speaking: Vec::new(),
            seated: false,
            joining: false,
        }
    }
}

fn ring_shown(state: &str) -> bool {
    matches!(state, "ringing" | "declined" | "missed" | "busy" | "unreachable")
}

/// What a refused group-call request says. A room's "That room is full." stays on the room path.
fn group_words(code: &str) -> &'static str {
    match code {
        "full" | "room_full" | "server_full" => "This call is full.",
        "not_found" => "That call has ended.",
        "busy" => "You're already in a call.",
        "slow_down" => "Too many tries. Wait a moment, then try again.",
        _ => "That didn't work. Try again.",
    }
}

fn rings_of(value: Option<&Value>) -> Vec<(String, String)> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = text(row, "user_id")?;
            if id.is_empty() {
                return None;
            }
            Some((id.to_owned(), text(row, "state").unwrap_or("ringing").to_owned()))
        })
        .take(SEATS)
        .collect()
}

fn ensure_group(seat: &mut Seat, group_id: &str, call_id: &str, starter: &str) {
    let group = seat.groups.entry(group_id.to_owned()).or_insert_with(|| GroupCall::new(call_id, starter));
    if group.call_id.is_empty() && !call_id.is_empty() {
        group.call_id = call_id.to_owned();
    }
    if group.starter.is_empty() && !starter.is_empty() {
        group.starter = starter.to_owned();
    }
}

/// The shell is in this group call, far enough along that a ring for it is not a new incoming call.
fn shell_shows(ctx: &Context, group_id: &str) -> bool {
    ctx.hub.calls.describe().0.is_some_and(|call| {
        call.conversation == ctx.key(group_id)
            && !matches!(
                call.state,
                CallState::Failed { .. } | CallState::RingingIn { .. } | CallState::RingingOut
            )
    })
}

/// `moved_from` names the direct this device is seated in. The direct seat stays until the move
/// retargets it; joining the group here would drop that media.
fn moved_here(seat: &Seat, frame: &Value) -> bool {
    let Some(from) = frame.get("moved_from").filter(|from| from.is_object()) else {
        return false;
    };
    let direct_id = text(from, "direct_id").unwrap_or("");
    if direct_id.is_empty() {
        return false;
    }
    let ring_id = text(from, "ring_id").unwrap_or("");
    seat.direct.as_ref().is_some_and(|direct| {
        direct.direct_id == direct_id && (ring_id.is_empty() || direct.ring_id.is_empty() || direct.ring_id == ring_id)
    })
}

fn hear_group(ctx: &Context, seat: &mut Seat, kind: &str, frame: &Value) {
    match kind {
        "call.group.started" => hear_started(ctx, seat, frame),
        "call.group.ring" => hear_ring_one(ctx, seat, frame),
        "call.group.joined" | "call.group.state" => hear_group_person(ctx, seat, frame, kind == "call.group.joined"),
        "call.group.left" => hear_group_left(ctx, seat, frame),
        "call.group.speaking" => hear_group_speaking(ctx, seat, frame),
        "call.group.ended" => {
            if let Some(group_id) = text(frame, "group_id") {
                end_group(ctx, seat, group_id);
            }
        }
        _ => {}
    }
}

/// A media frame whose room is `g:{call_id}`. Unknown groups are ignored, so a `call.joined`
/// for that room never becomes a voice channel.
fn hear_group_media(ctx: &Context, seat: &mut Seat, kind: &str, frame: &Value) {
    let Some(room) = text(frame, "room_id") else {
        return;
    };
    let Some(group_id) = group_for_media(seat, room) else {
        return;
    };
    match kind {
        "call.speaking" => {
            let users = speaking_ids(frame);
            if seat.wanted.as_deref() == Some(&group_id) {
                seat.speaking = users.clone();
            }
            if let Some(group) = seat.groups.get_mut(&group_id) {
                group.speaking = users;
            }
            publish_group(ctx, seat, &group_id);
        }
        "call.e2ee" if seat.call_id.is_some() && seat.wanted.as_deref() == Some(&group_id) => {
            ctx.tell_media(&group_id, MediaRequest::TinyE2ee { frame: frame.to_string() });
        }
        "call.slots" => hear_group_slots(ctx, seat, &group_id, frame),
        "call.state" => hear_group_person(ctx, seat, frame, false),
        _ => {}
    }
}

fn group_for_media(seat: &Seat, room: &str) -> Option<String> {
    let call_id = room.strip_prefix("g:")?;
    seat.groups.iter().find(|(_, group)| group.call_id == call_id).map(|(id, _)| id.clone())
}

fn speaking_ids(frame: &Value) -> Vec<String> {
    frame
        .get("user_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(SEATS)
        .map(str::to_owned)
        .collect()
}

fn hear_group_slots(ctx: &Context, seat: &Seat, group_id: &str, frame: &Value) {
    let current = seat.call_id.is_some() && text(frame, "call_id") == seat.call_id.as_deref();
    if !current || seat.wanted.as_deref() != Some(group_id) {
        return;
    }
    let audio = frame
        .get("audio")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(SEATS)
        .filter_map(|slot| {
            Some(TinySlot { mid: text(slot, "mid")?.to_owned(), user_id: text(slot, "user_id")?.to_owned() })
        });
    ctx.tiny_slots(group_id, audio.collect());
    let parse_pic = |video: &Value| -> Option<TinyPicture> {
        Some(TinyPicture {
            mid: text(video, "mid")?.to_owned(),
            user_id: text(video, "user_id")?.to_owned(),
            kind: if text(video, "kind") == Some("share") { PictureKind::Share } else { PictureKind::Camera },
        })
    };
    if let Some(videos) = frame.get("videos").and_then(Value::as_array) {
        let pictures: Vec<TinyPicture> = videos.iter().take(16).filter_map(parse_pic).collect();
        ctx.tell_media(group_id, MediaRequest::TinyPictures { pictures });
    } else {
        let picture = frame.get("video").filter(|video| !video.is_null()).and_then(parse_pic);
        ctx.tell_media(group_id, MediaRequest::TinyPicture { picture });
    }
}

fn hear_started(ctx: &Context, seat: &mut Seat, frame: &Value) {
    let Some(group_id) = text(frame, "group_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(call_id) = text(frame, "call_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let starter = text(frame, "started_by").unwrap_or("");
    let mut group = seat.groups.remove(group_id).unwrap_or_else(|| GroupCall::new(call_id, starter));
    group.call_id = call_id.to_owned();
    if !starter.is_empty() {
        group.starter = starter.to_owned();
    }
    if let Some(people) = frame.get("participants") {
        group.people = sittings(people);
    }
    if frame.get("rings").is_some() {
        group.rings = rings_of(frame.get("rings"));
    }
    let moved = moved_here(seat, frame);
    seat.groups.insert(group_id.to_owned(), group);
    publish_group(ctx, seat, group_id);
    maybe_ring_me(ctx, seat, group_id);
    if moved {
        return;
    }
    let starting = seat.starting.as_deref() == Some(group_id);
    let holding = shell_shows(ctx, group_id);
    let busy = seat.groups.get(group_id).is_some_and(|group| group.joining || group.seated);
    if (starting || holding) && !busy {
        seat.starting = None;
        seat.join_group = Some(group_id.to_owned());
    }
}

fn hear_ring_one(ctx: &Context, seat: &mut Seat, frame: &Value) {
    let Some(group_id) = text(frame, "group_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(user) = text(frame, "user_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let state = text(frame, "state").unwrap_or("ringing");
    let call_id = text(frame, "call_id").unwrap_or("");
    let starter = text(frame, "by").unwrap_or("");
    ensure_group(seat, group_id, call_id, starter);
    if let Some(group) = seat.groups.get_mut(group_id) {
        if let Some(existing) = group.rings.iter_mut().find(|(id, _)| id == user) {
            existing.1 = state.to_owned();
        } else if group.rings.len() < SEATS {
            group.rings.push((user.to_owned(), state.to_owned()));
        }
    }
    if user == seat.me {
        let seated = seat.groups.get(group_id).is_some_and(|group| group.seated);
        if state == "ringing" && !seated && !shell_shows(ctx, group_id) {
            let from = text(frame, "by")
                .filter(|id| !id.is_empty())
                .map(|id| person_name(seat, id))
                .or_else(|| {
                    seat.groups.get(group_id).filter(|group| !group.starter.is_empty()).map(|group| person_name(seat, &group.starter))
                })
                .unwrap_or_else(|| "Someone".to_owned());
            ctx.ring(group_id, &from);
        } else if state != "ringing" {
            ctx.ring_stopped(group_id);
        }
    }
    publish_group(ctx, seat, group_id);
}

fn hear_group_person(ctx: &Context, seat: &mut Seat, frame: &Value, joined: bool) {
    let Some(group_id) = text(frame, "group_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(user) = text(frame, "user_id").filter(|id| !id.is_empty()) else {
        return;
    };
    ensure_group(seat, group_id, text(frame, "call_id").unwrap_or(""), "");
    if let Some(group) = seat.groups.get_mut(group_id) {
        if let Some(who) = group.people.iter_mut().find(|who| who.user == user) {
            who.heard(frame);
        } else if joined && group.people.len() < SEATS {
            let mut who = Sitting::new(user);
            who.heard(frame);
            group.people.push(who);
        }
        if joined {
            group.rings.retain(|(id, _)| id != user);
        }
        if user == seat.me && joined {
            group.seated = true;
            group.joining = false;
        }
    }
    if user == seat.me && joined {
        seat.wanted = Some(group_id.to_owned());
        if let Some(id) = text(frame, "call_id").filter(|id| !id.is_empty()) {
            seat.call_id = Some(id.to_owned());
        } else if let Some(group) = seat.groups.get(group_id) {
            if !group.call_id.is_empty() {
                seat.call_id = Some(group.call_id.clone());
            }
        }
    }
    publish_group(ctx, seat, group_id);
}

fn hear_group_left(ctx: &Context, seat: &mut Seat, frame: &Value) {
    let Some(group_id) = text(frame, "group_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(user) = text(frame, "user_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let reason = text(frame, "reason").unwrap_or("left");
    if user == seat.me {
        if reason != "left" && shell_shows(ctx, group_id) {
            ctx.call_over(group_id);
        }
        group_unseat_me(ctx, seat, group_id);
        return;
    }
    if let Some(group) = seat.groups.get_mut(group_id) {
        group.people.retain(|who| who.user != user);
        group.rings.retain(|(id, _)| id != user);
    }
    publish_group(ctx, seat, group_id);
}

fn hear_group_speaking(ctx: &Context, seat: &mut Seat, frame: &Value) {
    let Some(group_id) = text(frame, "group_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let users = speaking_ids(frame);
    if seat.wanted.as_deref() == Some(group_id) {
        seat.speaking = users.clone();
    }
    if let Some(group) = seat.groups.get_mut(group_id) {
        group.speaking = users;
    }
    publish_group(ctx, seat, group_id);
}

fn end_group(ctx: &Context, seat: &mut Seat, group_id: &str) {
    seat.groups.remove(group_id);
    if seat.starting.as_deref() == Some(group_id) {
        seat.starting = None;
    }
    if seat.join_group.as_deref() == Some(group_id) {
        seat.join_group = None;
    }
    if seat.wanted.as_deref() == Some(group_id) {
        seat.wanted = None;
        seat.call_id = None;
        drop_watch(seat);
        seat.speaking.clear();
    }
    ctx.occupants(group_id, Vec::new(), 0);
    ctx.ring_stopped(group_id);
    if ctx.hub.calls.describe().0.is_some_and(|call| call.conversation == ctx.key(group_id)) {
        ctx.call_over(group_id);
    }
}

fn maybe_ring_me(ctx: &Context, seat: &Seat, group_id: &str) {
    let Some(group) = seat.groups.get(group_id) else {
        return;
    };
    if group.seated || shell_shows(ctx, group_id) {
        return;
    }
    if group.rings.iter().any(|(id, state)| id == &seat.me && state == "ringing") {
        let from = if group.starter.is_empty() { "Someone".to_owned() } else { person_name(seat, &group.starter) };
        ctx.ring(group_id, &from);
    }
}

fn group_unseat_me(ctx: &Context, seat: &mut Seat, group_id: &str) {
    let me = seat.me.clone();
    if let Some(group) = seat.groups.get_mut(group_id) {
        group.people.retain(|who| who.user != me);
        group.rings.retain(|(id, _)| id != &me);
        group.seated = false;
        group.joining = false;
    }
    if seat.wanted.as_deref() == Some(group_id) {
        seat.wanted = None;
        seat.call_id = None;
        drop_watch(seat);
        seat.speaking.clear();
    }
    publish_group(ctx, seat, group_id);
}

/// `ready.group_calls`. A value that is not a list leaves whatever this socket already knew.
fn restore_groups(ctx: &Context, seat: &mut Seat, value: &Value) {
    let Some(rows) = value.as_array() else {
        return;
    };
    let starting = seat.starting.clone();
    let mut next = HashMap::new();
    for row in rows.iter().take(ROOMS) {
        let Some(group_id) = text(row, "group_id").filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(call_id) = text(row, "call_id").filter(|id| !id.is_empty()) else {
            continue;
        };
        let mut group = GroupCall::new(call_id, text(row, "started_by").unwrap_or(""));
        if let Some(people) = row.get("participants") {
            group.people = sittings(people);
        }
        group.rings = rings_of(row.get("rings"));
        group.seated = group.people.iter().any(|who| who.user == seat.me);
        next.insert(group_id.to_owned(), group);
    }
    if let Some(id) = starting {
        if !next.contains_key(&id) {
            if let Some(old) = seat.groups.remove(&id) {
                next.insert(id, old);
            }
        }
    }
    let gone: Vec<String> = seat.groups.keys().filter(|id| !next.contains_key(*id)).cloned().collect();
    seat.groups = next;
    let ids: Vec<String> = seat.groups.keys().cloned().collect();
    for id in &ids {
        publish_group(ctx, seat, id);
        maybe_ring_me(ctx, seat, id);
    }
    for id in gone {
        ctx.occupants(&id, Vec::new(), 0);
        ctx.ring_stopped(&id);
    }
}

fn publish_group(ctx: &Context, seat: &Seat, group_id: &str) {
    let Some(group) = seat.groups.get(group_id) else {
        ctx.occupants(group_id, Vec::new(), 0);
        return;
    };
    let occupants: Vec<Occupant> = group
        .people
        .iter()
        .map(|who| Occupant {
            person: ctx.key(&who.user),
            name: person_name(seat, &who.user),
            avatar: None,
            muted: who.muted,
            deafened: who.deafened,
            server_muted: who.server_muted,
            server_deafened: who.server_deafened,
            suppressed: who.suppressed,
            streaming: who.sharing,
            camera: who.camera,
            speaking: group.speaking.iter().any(|id| id == &who.user),
        })
        .collect();
    let total = occupants.len() as u32;
    ctx.occupants(group_id, occupants, total);
    if !shell_shows(ctx, group_id) {
        return;
    }
    let others = group.people.iter().filter(|who| who.user != seat.me).map(|who| Participant {
        id: who.user.clone(),
        name: person_name(seat, &who.user),
        avatar: None,
        speaking: group.speaking.iter().any(|id| id == &who.user),
        muted: who.muted,
        deafened: who.deafened,
        server_muted: who.server_muted,
        server_deafened: who.server_deafened,
        suppressed: who.suppressed,
        priority: who.priority,
        camera: who.camera,
        sharing: who.sharing,
        ringing: false,
    });
    ctx.participants(group_id, others.collect());
    let ringing = group
        .rings
        .iter()
        .filter(|(id, state)| id != &seat.me && ring_shown(state))
        .map(|(id, state)| protocol::CallRing { id: id.clone(), name: person_name(seat, id), state: state.clone() })
        .collect();
    ctx.set_ringing(group_id, ringing);
}

fn note_group_join(seat: &mut Seat, group_id: &str, frame: &Value) {
    if let Some(id) = text(frame, "call_id").filter(|id| !id.is_empty()) {
        seat.call_id = Some(id.to_owned());
        if let Some(group) = seat.groups.get_mut(group_id) {
            if group.call_id.is_empty() {
                group.call_id = id.to_owned();
            }
        }
    } else if let Some(group) = seat.groups.get(group_id) {
        if !group.call_id.is_empty() {
            seat.call_id = Some(group.call_id.clone());
        }
    }
    let me = seat.me.clone();
    if let Some(group) = seat.groups.get_mut(group_id) {
        group.joining = false;
        group.seated = true;
        if !group.people.iter().any(|who| who.user == me) && group.people.len() < SEATS {
            group.people.push(Sitting::new(&me));
        }
    }
}

fn group_join_refused(ctx: &Context, seat: &mut Seat, group_id: &str) {
    if let Some(group) = seat.groups.get_mut(group_id) {
        group.joining = false;
        group.seated = false;
    }
    if seat.wanted.as_deref() == Some(group_id) {
        seat.wanted = None;
        seat.call_id = None;
    }
    ctx.call_over(group_id);
}

/// `None` rings every kept member who is not already seated and is not us. A named list is sent as it is.
fn expand_ring(saved: &Saved, seat: &Seat, group_id: &str, people: Option<Vec<String>>) -> Vec<String> {
    let Some(people) = people else {
        let seated = seat.groups.get(group_id).map(|group| group.people.iter().map(|who| who.user.clone()).collect::<Vec<_>>()).unwrap_or_default();
        return super::messages::group_member_ids(saved, group_id)
            .into_iter()
            .filter(|id| id != &seat.me && !seated.iter().any(|seated| seated == id))
            .collect();
    };
    people
}

#[allow(clippy::too_many_arguments)]
fn ask_group_start(
    wire: &mut Wire,
    pending: &mut Vec<Pending>,
    refs: u64,
    group_id: String,
    from_direct: Option<(String, String)>,
    reply: Reply<()>,
    timing: &Timing,
) -> io::Result<()> {
    let reference = refs.to_string();
    let mut frame = json!({"type": "call.group.start", "ref": reference, "group_id": group_id});
    if let Some((direct_id, ring_id)) = &from_direct {
        frame["from_direct"] = json!({"direct_id": direct_id, "ring_id": ring_id});
    }
    pending.push(Pending {
        reference,
        asked: Asked::GroupStart(group_id),
        until: Instant::now() + timing.answer,
        reply: Some(reply),
    });
    wire.send(frame.to_string())
}

#[allow(clippy::too_many_arguments)]
fn ask_group_join(
    wire: &mut Wire,
    pending: &mut Vec<Pending>,
    refs: u64,
    group_id: String,
    sdp: Option<Offered>,
    reply: Option<Reply<()>>,
    timing: &Timing,
) -> io::Result<()> {
    let reference = refs.to_string();
    let mut frame = json!({"type": "call.join", "ref": reference, "group_id": group_id});
    let media = sdp.is_some();
    if let Some((sdp, e2ee)) = sdp {
        frame["sdp"] = json!(sdp);
        if e2ee {
            frame["e2ee"] = json!(E2EE_VERSION);
        }
    }
    pending.push(Pending {
        reference,
        asked: Asked::Join(group_id, media, None),
        until: Instant::now() + timing.answer,
        reply,
    });
    wire.send(frame.to_string())
}

#[allow(clippy::too_many_arguments)]
fn ask_group_people(
    wire: &mut Wire,
    pending: &mut Vec<Pending>,
    refs: u64,
    seat: &Seat,
    group_id: String,
    people: Vec<String>,
    ring: bool,
    reply: Reply<()>,
    timing: &Timing,
) -> io::Result<()> {
    let call_id = seat.groups.get(&group_id).map(|group| group.call_id.clone()).unwrap_or_default();
    if call_id.is_empty() {
        let _ = reply.send(Err(Refusal::Refused("That call has ended.".to_owned())));
        return Ok(());
    }
    if people.is_empty() {
        let _ = reply.send(Err(Refusal::Refused("Nobody else to ring.".to_owned())));
        return Ok(());
    }
    let reference = refs.to_string();
    let kind = if ring { "call.group.ring" } else { "call.group.unring" };
    pending.push(Pending {
        reference: reference.clone(),
        asked: Asked::Group,
        until: Instant::now() + timing.answer,
        reply: Some(reply),
    });
    wire.send(json!({"type": kind, "ref": reference, "group_id": group_id, "call_id": call_id, "user_ids": people}).to_string())
}

#[allow(clippy::too_many_arguments)]
fn send_group_leave(
    ctx: &Context,
    wire: &mut Wire,
    pending: &mut Vec<Pending>,
    refs: u64,
    seat: &mut Seat,
    group_id: &str,
    timing: &Timing,
) -> io::Result<()> {
    let Some(group) = seat.groups.get(group_id) else {
        return Ok(());
    };
    let call_id = group.call_id.clone();
    if call_id.is_empty() {
        return Ok(());
    }
    let media = group.seated || group.joining || (seat.wanted.as_deref() == Some(group_id) && seat.call_id.is_some());
    let ringing = !media && group.rings.iter().any(|(id, state)| id == &seat.me && state == "ringing");
    if !media && !ringing {
        return Ok(());
    }
    let reference = refs.to_string();
    let frame = if media {
        json!({"type": "call.leave", "ref": reference, "group_id": group_id, "call_id": call_id})
    } else {
        json!({"type": "call.group.decline", "ref": reference, "group_id": group_id, "call_id": call_id})
    };
    pending.push(Pending {
        reference,
        asked: Asked::GroupLeave(group_id.to_owned()),
        until: Instant::now() + timing.answer,
        reply: None,
    });
    if !media {
        let me = seat.me.clone();
        if let Some(group) = seat.groups.get_mut(group_id) {
            group.rings.retain(|(id, _)| id != &me);
        }
        ctx.ring_stopped(group_id);
    }
    wire.send(frame.to_string())
}

#[allow(clippy::too_many_arguments)]
fn join_group_now(
    ctx: &Context,
    saved: &Saved,
    live: &Live,
    seat: &mut Seat,
    wire: &mut Wire,
    pending: &mut Vec<Pending>,
    refs: u64,
    group_id: &str,
    timing: &Timing,
) -> io::Result<()> {
    let Some(group) = seat.groups.get(group_id).cloned() else {
        return Ok(());
    };
    if group.call_id.is_empty() || group.joining || group.seated {
        return Ok(());
    }
    if let Some(group) = seat.groups.get_mut(group_id) {
        group.joining = true;
    }
    seat.wanted = Some(group_id.to_owned());
    if live.seats_only() {
        return ask_group_join(wire, pending, refs, group_id.to_owned(), None, None, timing);
    }
    match offer_group(ctx, saved, seat, group_id) {
        Ok(sdp) => {
            ask_group_join(wire, pending, refs, group_id.to_owned(), Some(sdp), None, timing)?;
            ctx.tiny_room(group_id, DIRECT_CALL_KBPS, false);
            ctx.tiny_video_allowed(group_id, true);
            Ok(())
        }
        Err(words) => {
            if let Some(group) = seat.groups.get_mut(group_id) {
                group.joining = false;
            }
            ctx.call_failed(group_id, CallFailure::Lost, &words);
            Ok(())
        }
    }
}

fn offer_group(ctx: &Context, saved: &Saved, seat: &mut Seat, group_id: &str) -> Result<Offered, String> {
    let Some(group) = seat.groups.get(group_id).cloned() else {
        return Err("That call has ended.".to_owned());
    };
    if group.call_id.is_empty() {
        return Err("That call has ended.".to_owned());
    }
    let host = server_host(saved);
    let required = must_encrypt(&host, false, seat.e2ee)?;
    let identity = crate::verified::identity_path(&ctx.account).to_string_lossy().into_owned();
    let e2ee = seat.e2ee.then(|| TinyE2ee { room_id: format!("g:{}", group.call_id), identity, required });
    let offered = e2ee.is_some();
    let proposal = TinyOffer {
        user_id: seat.me.clone(),
        host,
        slots: ROOM_CAP - 1,
        muted: seat.muted,
        deafened: seat.deafened,
        e2ee,
        video_slots: seat.video_slots,
    };
    let sdp = ctx.tiny_offer(group_id, proposal)?;
    seat.video_mids = parse_offer_video_mids(&sdp);
    Ok((sdp, offered))
}


