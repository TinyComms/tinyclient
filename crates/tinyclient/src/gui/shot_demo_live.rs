//! The demo's call, which moves by itself, so a demo build's call screen can be reached and
//! used by tapping: a call that is started rings, then connects with people who take turns to
//! speak; a voice channel connects at once with whoever sits in it. Mute, deafen, camera and
//! share are kept, and on a Tiny call the other person's camera comes on after a moment with
//! frames that change, as a real one's would. Every change is a `Change::Call`, handed to the
//! shell's `Wait`, the way chatd hands it.

use super::{find, members, photo, who};
use crate::view::now;
use protocol::{
    Call, CallPicture, CallState, Change, Content, ConversationKind, Direction, Key, MediaRequest, Network, Participant, PictureKind,
    Request, Response, VideoStream,
};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

struct State {
    seq: u64,
    call: Option<Call>,
    /// Counts calls, so a call's clock stops when another starts or it ends.
    run: u64,
    /// The other person's camera frames so far.
    frame: u64,
}

static STATE: Mutex<State> = Mutex::new(State { seq: 1, call: None, run: 0, frame: 0 });
static CHANGED: Condvar = Condvar::new();

/// What `Status` says the last change was, for the shell's first `Wait`.
pub fn seq() -> u64 {
    STATE.lock().map_or(1, |state| state.seq)
}

/// The call as it is now, for `Status`.
pub fn current() -> Option<Call> {
    STATE.lock().ok().and_then(|state| state.call.clone())
}

/// Changes the call, when `run` is still the call on (or always, without one), and wakes the
/// shell's `Wait`. False when that call is over.
fn change(run: Option<u64>, apply: impl FnOnce(&mut Option<Call>, &mut u64)) -> bool {
    let Ok(mut state) = STATE.lock() else {
        return false;
    };
    if run.is_some_and(|run| run != state.run) {
        return false;
    }
    let State { call, frame, .. } = &mut *state;
    apply(call, frame);
    state.seq += 1;
    CHANGED.notify_all();
    true
}

/// `Wait`: what changed since `since`, at once when it has, else as soon as it does, else a
/// quiet timeout the shell asks again after, as a quiet chatd's.
pub fn wait(since: u64) -> std::io::Result<Response> {
    let quiet = || std::io::Error::new(std::io::ErrorKind::TimedOut, "the demo is quiet");
    let state = STATE.lock().map_err(|_| quiet())?;
    let (state, _) = CHANGED.wait_timeout_while(state, Duration::from_secs(20), |state| state.seq <= since).map_err(|_| quiet())?;
    if state.seq <= since {
        return Err(quiet());
    }
    Ok(Response::Changes { seq: state.seq, reset: false, changes: vec![Change::Call { call: state.call.clone() }] })
}

fn person(id: &str) -> Participant {
    Participant { id: id.to_owned(), name: who(id).map_or(id, |who| who.name).to_owned(), ..Participant::default() }
}

fn bare(conversation: Key, state: CallState, participants: Vec<Participant>) -> Call {
    Call {
        conversation,
        state,
        participants,
        muted: false,
        deafened: false,
        joined: 0,
        input: "Microphone".to_owned(),
        output: "Speaker".to_owned(),
        engine_linked: true,
        video: Vec::new(),
        speaking: false,
        level: None,
        output_level: None,
        input_id: String::new(),
        output_id: String::new(),
        device_notice: None,
        device_stuck: false,
        inputs: Vec::new(),
        outputs: Vec::new(),
        security: None,
        picture: None,
    }
}

/// A call to `conversation`: a direct or group conversation rings, a voice channel joins.
fn start(conversation: &Key) -> Response {
    let Some(row) = find(conversation) else {
        return Response::Error { code: protocol::ErrorCode::NotFound, detail: "Not in the demo.".into() };
    };
    let voice = row.kind == ConversationKind::Voice;
    let people: Vec<Participant> = if voice {
        let inside = row.occupants.as_ref().map(|inside| inside.people.as_slice()).unwrap_or_default();
        inside.iter().map(|occupant| Participant { muted: occupant.muted, ..person(&occupant.person.id) }).collect()
    } else if let Some(peer) = &row.peer {
        vec![person(peer)]
    } else {
        members(conversation).iter().take(4).map(|member| person(&member.key.id)).collect()
    };
    let state = if voice { CallState::Connecting } else { CallState::RingingOut };
    let mut run = 0;
    change(None, |call, frame| {
        // A voice channel lists who is in it once we are; a ring lists whom it rings.
        *call = Some(bare(conversation.clone(), state, if voice { Vec::new() } else { people.clone() }));
        *frame = 0;
    });
    if let Ok(mut state) = STATE.lock() {
        state.run += 1;
        run = state.run;
    }
    let tiny = conversation.network == Network::Tiny;
    let peer = row.peer.clone();
    match std::thread::Builder::new().name("demo-call".into()).spawn(move || clock(run, voice, tiny, peer, people)) {
        Ok(_) => Response::Ok,
        Err(_) => Response::Error { code: protocol::ErrorCode::Unsupported, detail: "The demo could not ring.".into() },
    }
}

/// The call's own time: answered, then people talk in turn; on a Tiny call the other person's
/// camera comes on and its frames move.
fn clock(run: u64, voice: bool, tiny: bool, peer: Option<String>, people: Vec<Participant>) {
    std::thread::sleep(Duration::from_millis(if voice { 800 } else { 2_200 }));
    let answered = change(Some(run), |call, _| {
        if let Some(call) = call {
            call.state = CallState::Joined;
            call.joined = now();
            call.participants = people.clone();
        }
    });
    if !answered {
        return;
    }
    // The other camera's frames: pictures of the demo's own set, in turn.
    const FRAMES: [&str; 3] = ["preview-creek.webp", "weir-dawn.webp", "moss-frost.webp"];
    for tick in 1u64.. {
        std::thread::sleep(Duration::from_millis(1_400));
        let going = change(Some(run), |call, frame| {
            let Some(call) = call.as_mut().filter(|call| call.state == CallState::Joined) else {
                return;
            };
            // Who talks now: each in turn, with a breath of nobody between.
            let speaker = (tick % (call.participants.len() as u64 + 1)) as usize;
            for (ix, who) in call.participants.iter_mut().enumerate() {
                who.speaking = ix == speaker && !who.muted;
            }
            let Some(peer) = peer.as_deref().filter(|_| tiny && tick >= 2) else {
                return;
            };
            if let Some(who) = call.participants.iter_mut().find(|who| who.id == peer) {
                who.camera = true;
            }
            *frame += 1;
            call.picture = Some(CallPicture {
                path: photo(FRAMES[(*frame as usize) % FRAMES.len()]),
                width: 1280,
                height: 720,
                generation: *frame,
                user_id: peer.to_owned(),
                kind: PictureKind::Camera,
            });
        });
        if !going || current().is_none() {
            return;
        }
    }
}

/// Our own camera or share, on or off.
fn stream(call: &mut Call, content: Content, on: bool) {
    call.video.retain(|stream| !(stream.direction == Direction::Encode && stream.content == content));
    if on {
        call.video.push(VideoStream { direction: Direction::Encode, content, source: String::new() });
    }
}

/// My own camera's picture while it is on: the demo has no camera, so it is my portrait.
pub fn self_frame() -> Option<String> {
    let call = current()?;
    call.video.iter().any(|stream| stream.direction == Direction::Encode && stream.content == Content::Camera).then(|| photo("avatar-robin-vale.jpg"))
}

/// The call's requests: kept, and told back as the call's change. `None` for any other request.
pub fn answer(request: &Request) -> Option<Response> {
    let on_call = |apply: &dyn Fn(&mut Call)| {
        change(None, |call, _| {
            if let Some(call) = call {
                apply(call);
            }
        });
        Response::Ok
    };
    Some(match request {
        Request::CallStart { conversation } => start(conversation),
        Request::CallEnd | Request::CallDecline => {
            change(None, |call, _| *call = None);
            Response::Ok
        }
        Request::CallAccept => on_call(&|call| {
            call.state = CallState::Joined;
            call.joined = now();
        }),
        Request::CallSet { muted, deafened } | Request::Media(MediaRequest::Audio { muted, deafened }) => on_call(&|call| {
            (call.muted, call.deafened) = (*muted || *deafened, *deafened);
            call.speaking &= !call.muted;
        }),
        Request::CallTalk { held, .. } => on_call(&|call| call.speaking = *held && !call.muted),
        Request::Media(MediaRequest::Camera { on }) => on_call(&|call| stream(call, Content::Camera, *on)),
        Request::Media(MediaRequest::ShareSend { on }) => on_call(&|call| stream(call, Content::Share, *on)),
        Request::Media(_) => Response::Ok,
        _ => return None,
    })
}
