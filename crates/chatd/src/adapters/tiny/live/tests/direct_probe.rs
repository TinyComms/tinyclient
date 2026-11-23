//! Real two-process probe against an owned disposable tinyserver. No desktop or sound device.
//! TINY_DIRECT_CALL_FIXTURE points to a private JSON file with base_url and synthetic owner/peer
//! { user_id, token, display_name }. The server must expose API and its ICE-TCP candidate.
//! Run: cargo test -p chatd --no-default-features --lib real_direct_call_round_trip -- --ignored --nocapture

use super::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const WAIT: Duration = Duration::from_secs(45);

fn wait_for(what: &str, check: impl Fn() -> bool) {
    wait_for_within(what, WAIT, check);
}

fn wait_for_within(what: &str, limit: Duration, mut check: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
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
    let out = run_peers("real_direct_call_peer", false);
    for name in ["owner-no-media", "peer-no-media", "owner-audio", "peer-audio", "peer-muted", "peer-unmuted", "peer-left", "peer-declined", "owner-declined", "peer-cancelled"] {
        assert!(flagged(out.path(), name), "missing actual proof: {name}");
    }
    println!("direct-call-proof: no media before accept; bidirectional PCM; mute; leave; decline; cancel");
}

fn run_peers(test: &str, video: bool) -> tempfile::TempDir {
    let fixture = std::env::var_os("TINY_DIRECT_CALL_FIXTURE").expect("TINY_DIRECT_CALL_FIXTURE private JSON");
    let out = tempfile::tempdir().expect("probe artifacts");
    let mut children = Children(Vec::new());
    for side in ["owner", "peer"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--ignored", "--exact", &format!("adapters::tiny::live::tests::direct_probe::{test}"), "--nocapture"])
            .env("TINY_DIRECT_CALL_FIXTURE", &fixture)
            .env("TINY_DIRECT_CALL_SIDE", side)
            .env("TINY_DIRECT_CALL_OUT", out.path())
            .env("TINYCLIENT_DIR", out.path().join(format!("{side}-data")))
            .env("TINYCLIENT_MEDIAD_AUDIO", "tone")
            .env("TINYCLIENT_MEDIAD_ICE", "tcp")
            .env("TINYCLIENT_MEDIAD_SINK", out.path().join(format!("{side}.pcm")))
            .stdout(Stdio::from(std::fs::File::create(out.path().join(format!("{side}.log"))).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(out.path().join(format!("{side}.err"))).unwrap()));
        if video {
            command.env("TINYCLIENT_MEDIAD_PICTURE", "pattern").env("TINYCLIENT_LOG_LEVEL", "info");
        }
        protocol::launch::no_console(&mut command);
        children.0.push(command.spawn().expect("probe child"));
    }
    let deadline = Instant::now() + Duration::from_secs(if video { 180 } else { 110 });
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
    out
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

fn probe_identity() -> (String, PathBuf, Value, &'static str, Saved) {
    let side = std::env::var("TINY_DIRECT_CALL_SIDE").expect("probe side");
    let out = PathBuf::from(std::env::var_os("TINY_DIRECT_CALL_OUT").unwrap());
    let fixture: Value = serde_json::from_slice(&std::fs::read(std::env::var_os("TINY_DIRECT_CALL_FIXTURE").unwrap()).unwrap()).unwrap();
    let other = if side == "owner" { "peer" } else { "owner" };
    let saved = super::super::super::parse(&json!({
        "base_url": fixture["base_url"], "token": fixture[&side]["token"], "user_id": fixture[&side]["user_id"],
        "display_name": fixture[&side]["display_name"], "email": format!("{side}@example.test"), "role": if side == "owner" { "owner" } else { "member" }
    }).to_string()).unwrap();
    (side, out, fixture, other, saved)
}

#[test]
#[ignore = "needs a disposable real server and two owned synthetic sessions"]
fn real_video_round_trip() {
    run_peers("real_video_peer", true);
    println!("video-received-proof: room share+camera painted; direct share+camera painted; counters advance; cleared on stop");
}

#[test]
#[ignore = "child of real_video_round_trip"]
fn real_video_peer() {
    with_mediad();
    let (side, out, fixture, other, saved) = probe_identity();
    let other_id = fixture[other]["user_id"].as_str().expect("other user's id");
    let owner = side == "owner";
    let (running, ctx) = real_client(&saved);
    wait_for("live connection and media capability", || running.link().0 == Link::Connected && !running.live.seats_only());
    flag(&out, &format!("{side}-ready"));
    wait_for("other live connection", || flagged(&out, &format!("{other}-ready")));

    for scope in ["room", "direct"] {
        let id = if owner {
            let id = if scope == "room" {
                let (status, body) = super::super::super::asked("POST", &saved, "/v1/rooms",
                    Some(json!({"name": "Proof", "video_enabled": true}))).expect("create Proof room");
                assert_eq!(status, 201, "create Proof room");
                body["id"].as_str().expect("room id").to_owned()
            } else {
                let (reply, result) = channel();
                super::super::super::messages::serve(&ctx, &saved, &running.live,
                    crate::adapters::Command::StartDirectCall { person_id: other_id.into(), reply });
                result.recv_timeout(WAIT).unwrap().expect("fixture direct")
            };
            std::fs::write(out.join(scope), &id).unwrap();
            flag(&out, &format!("{scope}-created"));
            id
        } else {
            wait_for("call created", || flagged(&out, &format!("{scope}-created")));
            std::fs::read_to_string(out.join(scope)).unwrap()
        };
        if scope == "room" {
            assert_eq!(running.ask_within(WAIT, |reply| Ask::Join { room_id: id.clone(), reply }), Ok(()));
        } else if owner {
            ring(&running, &id);
        } else {
            wait_for("incoming direct ring", || incoming(&running));
            assert_eq!(running.ask_within(WAIT, |reply| Ask::Accept { direct_id: id.clone(), reply }), Ok(()));
        }
        let call = || running.hub.calls.describe().0.filter(|call| call.conversation == running.key(&id));
        wait_for(&format!("{scope} joined with end-to-end protection and the other member"), || {
            call().is_some_and(|call| call.state == protocol::CallState::Joined
                && call.security.is_some_and(|security| security.mode == protocol::SecurityMode::EndToEnd
                    && security.members.iter().any(|member| member.user_id == other_id)))
        });
        flag(&out, &format!("{scope}-{side}-joined"));
        wait_for("both joined", || flagged(&out, &format!("{scope}-{other}-joined")));
        for kind in [PictureKind::Share, PictureKind::Camera] {
            let label = format!("{scope}-{kind:?}");
            if owner {
                let send = |on| match kind {
                    PictureKind::Share => MediaRequest::ShareSend { on, screen: None, quality: None, show_cursor: None, sound: None, preview: None },
                    PictureKind::Camera => MediaRequest::Camera { on },
                };
                running.hub.calls.request(&running.hub, &send(true)).unwrap();
                wait_for(&format!("{label}: encode and local preview"), || call().is_some_and(|call| {
                    let (content, preview) = match kind {
                        PictureKind::Share => (protocol::Content::Share, call.share_view),
                        PictureKind::Camera => (protocol::Content::Camera, call.self_view),
                    };
                    preview.is_some() && call.video.iter().any(|stream| stream.content == content && stream.direction == protocol::Direction::Encode)
                }));
                prove_codec_stats(&running, &label, true);
                wait_for(&format!("{label}: viewer proved motion"), || flagged(&out, &format!("{label}-moving")));
                running.hub.calls.request(&running.hub, &send(false)).unwrap();
            } else {
                let picture = || call().and_then(|call| call.picture).filter(|p| p.user_id == other_id && p.kind == kind);
                wait_for(&format!("{label}: sender advertises video"), || call().is_some_and(|call| {
                    call.participants.iter().any(|who| who.id == other_id && match kind {
                        PictureKind::Share => who.sharing,
                        PictureKind::Camera => who.camera,
                    })
                }));
                // The shell owns direct auto-watch. This headless peer expresses the same
                // primary selection through Tiny's live Watch path (including multi-video SFUs).
                // The windowed scenario checks that the actual shell applies it on its own.
                if !(scope == "room" && kind == PictureKind::Share && std::env::var_os("TINY_VIDEO_SKIP_ROOM_WATCH").is_some()) {
                    assert_eq!(running.ask(|reply| Ask::Watch {
                        sources: vec![protocol::WatchSource { user_id: other_id.into(), kind, size: protocol::WatchSize::Large }],
                        reply,
                    }), Ok(()));
                }
                prove_moving_picture(&label, picture);
                prove_codec_stats(&running, &label, false);
                assert!(call().unwrap().video.iter().any(|stream| stream.direction == protocol::Direction::Decode && stream.source == other_id),
                    "{label}: viewer's decode session is missing");
                flag(&out, &format!("{label}-moving"));
                wait_for_within(&format!("{label}: viewer picture clears after sender stops"), Duration::from_secs(5),
                    || call().is_some_and(|call| call.picture.is_none()));
                assert_eq!(running.ask(|reply| Ask::Watch { sources: Vec::new(), reply }), Ok(()));
                let log = std::fs::read_to_string(protocol::log_policy::logs_dir().join("mediad.log")).expect("viewer mediad.log");
                assert!(log.lines().any(|line| line.contains("first keyframe received")
                    && line.contains(&format!("kind={kind:?}"))), "{label}: viewer log has no first keyframe");
                flag(&out, &format!("{label}-cleared"));
            }
            wait_for(&format!("{label}: viewer confirms clear"), || flagged(&out, &format!("{label}-cleared")));
        }
        if scope == "room" {
            assert_eq!(running.ask(|reply| Ask::Leave { reply }), Ok(()));
            wait_for("room call ended", || call().is_none());
            flag(&out, &format!("room-{side}-left"));
            if owner {
                wait_for("viewer left room", || flagged(&out, "room-peer-left"));
                let (status, _) = super::super::super::asked("DELETE", &saved, &format!("/v1/rooms/{id}"), None)
                    .expect("delete Proof room");
                assert_eq!(status, 204, "delete Proof room");
            }
        } else if owner {
            end(&running, &id);
        }
    }
}

/// The codec lab checks the stats returned through the real call daemon while the
/// track is running. Ordinary runs of the picture probe keep their existing contract.
fn prove_codec_stats(running: &Running, label: &str, sending: bool) {
    let Ok(mode) = std::env::var("TINY_GPU_CODECS_PROOF") else { return };
    let direction = if sending { "encoder" } else { "decoder" };
    let expect = if mode == "software" {
        Some("OpenH264".to_owned())
    } else {
        std::env::var(if sending { "TINY_GPU_EXPECT_ENCODER" } else { "TINY_GPU_EXPECT_DECODER" }).ok()
    };
    let allowed: &[&str] = if cfg!(windows) {
        if sending { &["Media Foundation"] } else { &["Media Foundation (D3D11)"] }
    } else if cfg!(target_os = "macos") {
        &["VideoToolbox"]
    } else if sending {
        &["VA-API", "NVENC"]
    } else {
        &["VA-API", "NVDEC"]
    };
    let mut names = Vec::new();
    wait_for_within(&format!("{label}: {mode} {direction} backend in call stats"), Duration::from_secs(8), || {
        let Ok(protocol::MediaResponse::Stats(stats)) = running.hub.calls.request(&running.hub, &MediaRequest::Stats) else {
            return false;
        };
        let Some(video) = stats.video else { return false };
        names = if sending { video.encoder_backends } else { video.decoder_backends };
        match &expect {
            Some(expected) if mode == "software" => names == [expected.as_str()],
            Some(expected) => names.contains(expected),
            None => !names.is_empty() && names.iter().all(|name| name == "OpenH264" || allowed.contains(&name.as_str())),
        }
    });
    println!("gpu-codecs-proof: {mode} {label} {direction}: {}", names.join(", "));
    if let Some(evidence) = std::env::var_os("TINY_VIDEO_EVIDENCE") {
        std::fs::write(PathBuf::from(evidence).join(format!("{label}-{direction}.txt")), names.join(", "))
            .expect("save the backend from CallStats");
    }
}

/// Decode the same JPEG the window reads. The blocks are read from their centers to avoid
/// JPEG and scaling edge artifacts.
fn pattern_counter(picture: &protocol::CallPicture, bytes: &[u8]) -> Option<u16> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?.to_rgb8();
    let (w, h) = image.dimensions();
    if (w, h) != (picture.width, picture.height) || w < 32 || h < 16 {
        return None;
    }
    let palette = [[224i16, 48, 48], [48, 80, 224], [48, 192, 64]];
    let matches = |pixel: [u8; 3], color: [i16; 3]| pixel.into_iter().zip(color).all(|(a, b)| (i16::from(a) - b).abs() < 40);
    if !palette.into_iter().any(|color| {
        [3, 5, 7].into_iter().all(|x| matches(image.get_pixel(w * x / 10, h / 2).0, color))
    }) {
        return None;
    }
    let mut counter = 0u16;
    for bit in 0..16 {
        let pixel = image.get_pixel(w * (2 * bit + 1) / 32, h / 8).0;
        if pixel.iter().all(|&channel| channel > 200) {
            counter |= 1 << bit;
        } else if pixel.iter().any(|&channel| channel > 55) {
            return None;
        }
    }
    Some(counter)
}

fn prove_moving_picture(label: &str, picture: impl Fn() -> Option<protocol::CallPicture>) {
    let evidence = std::env::var_os("TINY_VIDEO_EVIDENCE").map(|path| PathBuf::from(path).join(label));
    if let Some(dir) = &evidence {
        std::fs::create_dir_all(dir).expect("create viewer evidence directory");
    }
    // A replaced file is a missed sample, so try the next generation. Keep only the actual
    // byte stream we decoded, never a second read of a path the painter may have replaced.
    let sample = |p: &protocol::CallPicture, index| {
        let bytes = std::fs::read(&p.path).ok()?;
        let counter = pattern_counter(p, &bytes)?;
        if let Some(dir) = &evidence {
            std::fs::write(dir.join(format!("{index:04}.jpg")), bytes).expect("save viewer JPEG");
        }
        Some(counter)
    };
    let mut first = None;
    wait_for_within(&format!("{label}: first decoded pattern JPEG"), Duration::from_secs(10), || {
        first = picture().and_then(|p| sample(&p, 0).map(|counter| (p.generation, counter)));
        first.is_some()
    });
    let (mut generation, first_counter) = first.unwrap();
    let mut advances = 0;
    let mut counter_advanced = false;
    wait_for_within(&format!("{label}: ten new JPEG generations and a changing pattern counter"), Duration::from_secs(5), || {
        if let Some(p) = picture().filter(|p| p.generation > generation) {
            if let Some(counter) = sample(&p, advances + 1) {
                generation = p.generation;
                advances += 1;
                counter_advanced |= counter > first_counter;
            }
        }
        advances >= 10 && counter_advanced
    });
}

#[test]
#[ignore = "child of real_direct_call_round_trip"]
fn real_direct_call_peer() {
    with_mediad();
    let (side, out, fixture, other, saved) = probe_identity();
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
