//! Real two-process probe against an owned disposable tinyserver. No desktop or sound device.
//! TINY_DIRECT_CALL_FIXTURE points to a private JSON file with base_url and synthetic owner/peer
//! { user_id, token, display_name }. The server must expose API and its ICE-TCP candidate.
//! Run: cargo test -p chatd --no-default-features --lib real_direct_call_round_trip -- --ignored --nocapture

use super::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const WAIT: Duration = Duration::from_secs(45);

fn wait_for(what: &str, check: impl Fn() -> bool) {
    let until = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn flag(out: &Path, name: &str) { std::fs::write(out.join(name), b"ok").unwrap(); }
fn flagged(out: &Path, name: &str) -> bool { out.join(name).is_file() }

struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 { let _ = child.kill(); let _ = child.wait(); }
    }
}

#[test]
#[ignore = "needs a disposable real server and two owned synthetic sessions"]
fn real_direct_call_round_trip() {
    let fixture = std::env::var_os("TINY_DIRECT_CALL_FIXTURE").expect("TINY_DIRECT_CALL_FIXTURE private JSON");
    let out = tempfile::tempdir().expect("probe artifacts");
    let mut children = Children(Vec::new());
    for side in ["owner", "peer"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--ignored", "--exact", "adapters::tiny::live::tests::direct_probe::real_direct_call_peer", "--nocapture"])
            .env("TINY_DIRECT_CALL_FIXTURE", &fixture)
            .env("TINY_DIRECT_CALL_SIDE", side)
            .env("TINY_DIRECT_CALL_OUT", out.path())
            .env("TINYCLIENT_DIR", out.path().join(format!("{side}-data")))
            .env("TINYCLIENT_MEDIAD_AUDIO", "tone")
            .env("TINYCLIENT_MEDIAD_ICE", "tcp")
            .env("TINYCLIENT_MEDIAD_SINK", out.path().join(format!("{side}.pcm")))
            .stdout(Stdio::from(std::fs::File::create(out.path().join(format!("{side}.log"))).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(out.path().join(format!("{side}.err"))).unwrap()));
        protocol::launch::no_console(&mut command);
        children.0.push(command.spawn().expect("probe child"));
    }
    let deadline = Instant::now() + Duration::from_secs(110);
    loop {
        let statuses: Vec<_> = children.0.iter_mut().map(|child| child.try_wait().unwrap()).collect();
        if statuses.iter().all(Option::is_some) || statuses.iter().flatten().any(|status| !status.success()) || Instant::now() >= deadline {
            for side in ["owner", "peer"] {
                print!("{}", std::fs::read_to_string(out.path().join(format!("{side}.log"))).unwrap_or_default());
                eprint!("{}", std::fs::read_to_string(out.path().join(format!("{side}.err"))).unwrap_or_default());
            }
            assert!(statuses.iter().all(|status| status.is_some_and(|status| status.success())), "both probe processes must pass: {statuses:?}");
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for name in ["owner-no-media", "peer-no-media", "owner-audio", "peer-audio", "peer-muted", "peer-unmuted", "peer-left", "peer-declined", "owner-declined", "peer-cancelled"] {
        assert!(flagged(out.path(), name), "missing actual proof: {name}");
    }
    println!("direct-call-proof: no media before accept; bidirectional PCM; mute; leave; decline; cancel");
}

fn real_client(saved: &Saved) -> (Running, crate::adapters::Context) {
    let hub = Arc::new(Hub::new(Store::open(Path::new(":memory:")).unwrap()));
    hub.set_setting("", protocol::settings::NOISE_SUPPRESSION, Some("standard".into())).unwrap();
    let account = AccountKey { network: Network::Tiny, account: saved.user_id.clone() };
    let (adapter, ctx, _commands) = local(account.clone(), hub.clone());
    ctx.link(Link::Offline);
    let live = Arc::new(Live::new().unwrap());
    let notify = live.clone();
    hub.calls.on_signal(move |_, frame| notify.ask(Ask::E2ee { frame }));
    let notify = live.clone();
    hub.calls.on_offer(move |_, sdp| notify.ask(Ask::Offer { sdp }));
    let notify = live.clone();
    hub.calls.on_tiny_down(move |_| notify.ask(Ask::Resume));
    let notify = live.clone();
    hub.calls.on_tiny_up(move |_| notify.ask(Ask::Settled));
    let notify = live.clone();
    hub.calls.on_failure(move |key| notify.ask(Ask::Hangup { room_id: key.id.clone() }));
    let (socket_ctx, socket, socket_live) = (ctx.clone(), saved.clone(), live.clone());
    let thread = std::thread::spawn(move || keep_live(&socket_ctx, &socket, &socket_live, &SERVER));
    (Running { hub, account, live, adapter: Some(adapter), thread: Some(thread) }, ctx)
}

fn ring(running: &Running, direct: &str) {
    running.hub.calls.place(&running.hub, running.key(direct)).unwrap();
    assert_eq!(running.ask(|reply| Ask::Ring { attempt: None, direct_id: direct.into(), reply }), Ok(()));
}
fn end(running: &Running, direct: &str) {
    running.hub.calls.end(&running.hub);
    running.live.ask(Ask::Hangup { room_id: direct.into() });
}
fn incoming(running: &Running) -> bool {
    running.hub.calls.describe().0.is_some_and(|call| matches!(call.state, protocol::CallState::RingingIn { .. }))
}

#[test]
#[ignore = "child of real_direct_call_round_trip"]
fn real_direct_call_peer() {
    with_mediad();
    let side = std::env::var("TINY_DIRECT_CALL_SIDE").expect("probe side");
    let out = PathBuf::from(std::env::var_os("TINY_DIRECT_CALL_OUT").unwrap());
    let fixture: Value = serde_json::from_slice(&std::fs::read(std::env::var_os("TINY_DIRECT_CALL_FIXTURE").unwrap()).unwrap()).unwrap();
    let other = if side == "owner" { "peer" } else { "owner" };
    let saved = super::super::super::parse(&json!({
        "base_url": fixture["base_url"], "token": fixture[&side]["token"], "user_id": fixture[&side]["user_id"],
        "display_name": fixture[&side]["display_name"], "email": format!("{side}@example.test"), "role": if side == "owner" { "owner" } else { "member" }
    }).to_string()).unwrap();
    if side == "owner" {
        let (_, directs) = super::super::super::asked("GET", &saved, "/v1/directs", None).expect("fixture direct list");
        let rows = directs.get("directs").unwrap_or(&directs).as_array().expect("direct list");
        assert!(rows.iter().all(|row| row.pointer("/peer/user_id") != Some(&fixture[other]["user_id"])), "the disposable fixture pair must have no direct yet");
    }
    let (running, ctx) = real_client(&saved);
    wait_for("live connection and media capability", || running.link().0 == Link::Connected && !running.live.seats_only());
    assert!(super::super::super::e2ee::started(&saved).is_none(), "call crypto is independent of a text device");
    flag(&out, &format!("{side}-ready"));
    let direct = if side == "owner" {
        wait_for("other live connection", || flagged(&out, "peer-ready"));
        let (reply, result) = channel();
        super::super::super::messages::serve(&ctx, &saved, &running.live,
            crate::adapters::Command::StartDirectCall { person_id: fixture[other]["user_id"].as_str().unwrap().into(), reply });
        let direct = result.recv_timeout(WAIT).unwrap().unwrap();
        assert_eq!(ctx.known(&direct).unwrap().unwrap().call, protocol::CallSupport::Native);
        std::fs::write(out.join("direct"), &direct).unwrap();
        direct
    } else {
        wait_for("created direct", || out.join("direct").is_file());
        std::fs::read_to_string(out.join("direct")).unwrap()
    };
    let call = || running.hub.calls.describe().0.filter(|call| call.conversation == running.key(&direct));
    if side == "owner" { ring(&running, &direct); }
    wait_for("incoming or outgoing ring", || call().is_some_and(|call| matches!(call.state, protocol::CallState::RingingOut | protocol::CallState::RingingIn { .. })));
    assert!(!running.hub.calls.describe().1, "ringing never starts mediad");
    assert!(!out.join(format!("{side}.pcm")).is_file(), "ringing never opens the audio sink");
    flag(&out, &format!("{side}-no-media"));
    wait_for("both rings have no media", || flagged(&out, "owner-no-media") && flagged(&out, "peer-no-media"));
    if side == "peer" {
        assert_eq!(running.ask_within(WAIT, |reply| Ask::Accept { direct_id: direct.clone(), reply }), Ok(()));
    }
    let sink = out.join(format!("{side}.pcm"));
    wait_for("joined call and actual received tone", || {
        call().is_some_and(|call| call.state == protocol::CallState::Joined
            && call.security.is_some_and(|security| security.mode == protocol::SecurityMode::EndToEnd
                && !security.code.is_empty()
                && security.members.iter().any(|member| member.user_id == fixture[other]["user_id"])))
            && (2500. ..7500.).contains(&played_level(&sink))
    });
    let security = call().unwrap().security.unwrap();
    std::fs::write(out.join(format!("{side}-call-code")), &security.code).unwrap();
    println!("{side} received end-to-end encrypted tone RMS {:.0}", played_level(&sink));
    assert!(running.hub.occupants(&running.key(&direct)).is_none(), "direct calls create no room occupants");
    flag(&out, &format!("{side}-audio"));
    wait_for("bidirectional PCM", || flagged(&out, "owner-audio") && flagged(&out, "peer-audio"));
    wait_for("other end-to-end group authenticator", || out.join(format!("{other}-call-code")).is_file());
    assert_eq!(std::fs::read_to_string(out.join(format!("{other}-call-code"))).unwrap(), security.code, "both clients hold the same end-to-end group");
    if side == "owner" {
        running.hub.calls.set_audio(&running.hub, true, false).unwrap();
        running.live.ask(Ask::State { muted: true, deafened: false });
        wait_for("peer observes mute", || flagged(&out, "peer-muted"));
        running.hub.calls.set_audio(&running.hub, false, false).unwrap();
        running.live.ask(Ask::State { muted: false, deafened: false });
        wait_for("peer observes unmute", || flagged(&out, "peer-unmuted"));
        end(&running, &direct);
        wait_for("peer observes leave", || flagged(&out, "peer-left"));
        ring(&running, &direct);
        wait_for("decline", || call().is_some_and(|call| matches!(call.state, protocol::CallState::Failed { reason: protocol::CallFailure::NoAnswer, .. })));
        assert!(!running.hub.calls.describe().1);
        flag(&out, "owner-declined");
        end(&running, &direct);
        wait_for("peer declined", || flagged(&out, "peer-declined"));
        ring(&running, &direct);
        wait_for("peer receives last ring", || flagged(&out, "peer-last-ring"));
        end(&running, &direct);
        wait_for("peer observes cancel", || flagged(&out, "peer-cancelled"));
    } else {
        wait_for("remote mute", || call().is_some_and(|call| call.participants.iter().any(|who| who.id == fixture[other]["user_id"] && who.muted)));
        flag(&out, "peer-muted");
        wait_for("remote unmute", || call().is_some_and(|call| call.participants.iter().any(|who| who.id == fixture[other]["user_id"] && !who.muted)));
        flag(&out, "peer-unmuted");
        wait_for("call ends after other party leaves", || call().is_none() && !running.hub.calls.describe().1);
        flag(&out, "peer-left");
        wait_for("second incoming ring", || incoming(&running));
        end(&running, &direct);
        assert!(!running.hub.calls.describe().1);
        flag(&out, "peer-declined");
        wait_for("third incoming ring", || incoming(&running));
        flag(&out, "peer-last-ring");
        wait_for("caller cancels", || call().is_none());
        assert!(!running.hub.calls.describe().1);
        flag(&out, "peer-cancelled");
    }
}
