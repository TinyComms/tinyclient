//! Two real mediad processes in one Tiny server call, through a small SFU made here on str0m:
//! ICE-lite, a UDP socket and a passive ICE-TCP listener on the same port, and each person's
//! Opus copied to the other's receive slot. The test plays chatd: it carries each offer to the
//! fake and the answer and slot map back, as chatd does over the server's live socket.
//!
//! No sound device is opened. `TINYCLIENT_MEDIAD_AUDIO=tone` gives each mediad a 440 Hz tone to
//! say, and `TINYCLIENT_MEDIAD_SINK` a file for what it plays: each must hear the other's tone.
//! `TINYCLIENT_MEDIAD_ICE` makes them offer only UDP, or only TCP, as on a network that blocks
//! UDP.
//!
//! The same flow against a real tinyserver is `tiny_server_call` in chatd's tests, ignored by
//! default because it needs the server running.

// The server's MLS side, for the end-to-end encrypted calls below.
#[allow(dead_code)]
#[path = "support/mls.rs"]
mod mls;

use mediad::framing::Conn;
use mio::net::{TcpListener, UdpSocket};
use mio::{Events, Interest, Poll, Token};
use protocol::{read_frame, write_frame, CallPath, Cue, InputMode, MediaRequest, MediaResponse, SecurityMode, TinyE2ee, TinyOffer, TinySlot};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};
use str0m::change::SdpOffer;
use str0m::media::{MediaData, Mid};
use str0m::net::{Protocol, Receive, TcpType};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig};

const PATIENCE: Duration = Duration::from_secs(15);
const UDP: Token = Token(0);
const LISTENER: Token = Token(1);

// ---- the fake SFU ----

struct Peer {
    name: String,
    rtc: Rtc,
    /// Their microphone.
    publish: Option<Mid>,
    /// Their receive slots, in the offer's order.
    slots: Vec<Mid>,
    connected: bool,
}

/// What the test can see of the fake.
#[derive(Default)]
struct Seen {
    /// Frames heard from each person, by name.
    frames: HashMap<String, usize>,
    /// Which protocols carried each person's RTP.
    over: HashMap<String, Vec<Protocol>>,
    /// Frames from each person sealed end to end: DAVE's marker ends them.
    sealed: HashMap<String, usize>,
}

struct Join {
    name: String,
    offer: String,
    answer: Sender<(String, Vec<String>)>,
}

struct Sfu {
    joins: Sender<Join>,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sfu {
    fn start() -> Sfu {
        // One port for both. The system may hold the port UDP got for TCP (Windows reserves
        // ranges of them, refusing with "access denied"): then another is tried.
        let (mut udp, addr, mut listener) = (0..50)
            .find_map(|_| {
                let udp = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).ok()?;
                let addr = udp.local_addr().ok()?;
                Some((udp, addr, TcpListener::bind(addr).ok()?))
            })
            .expect("a port free for both UDP and TCP");
        let poll = Poll::new().unwrap();
        poll.registry().register(&mut udp, UDP, Interest::READABLE).unwrap();
        poll.registry().register(&mut listener, LISTENER, Interest::READABLE).unwrap();
        let (joins, asked) = channel();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut fake = Fake { udp, listener, addr, poll, peers: Vec::new(), streams: HashMap::new(), next: 2, seen: seen.clone() };
        let halt = stop.clone();
        let thread = std::thread::spawn(move || fake.run(&asked, &halt));
        Sfu { joins, seen, stop, thread: Some(thread) }
    }

    /// The answer to `offer`, and the receive slots it names, in order.
    fn join(&self, name: &str, offer: &str) -> (String, Vec<String>) {
        let (answer, answered) = channel();
        self.joins.send(Join { name: name.into(), offer: offer.into(), answer }).unwrap();
        answered.recv_timeout(PATIENCE).expect("the fake answered")
    }

    fn frames(&self, name: &str) -> usize {
        self.seen.lock().unwrap().frames.get(name).copied().unwrap_or(0)
    }

    fn sealed(&self, name: &str) -> usize {
        self.seen.lock().unwrap().sealed.get(name).copied().unwrap_or(0)
    }

    fn over(&self, name: &str) -> Vec<Protocol> {
        self.seen.lock().unwrap().over.get(name).cloned().unwrap_or_default()
    }
}

impl Drop for Sfu {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Fake {
    udp: UdpSocket,
    listener: TcpListener,
    addr: SocketAddr,
    poll: Poll,
    peers: Vec<Peer>,
    /// Accepted streams by token: the client's address, the framing, and whose it is once a
    /// STUN check said so.
    streams: HashMap<Token, (SocketAddr, Conn, Option<usize>)>,
    next: usize,
    seen: Arc<Mutex<Seen>>,
}

impl Fake {
    fn run(&mut self, joins: &Receiver<Join>, stop: &AtomicBool) {
        let mut events = Events::with_capacity(64);
        while !stop.load(Ordering::Relaxed) {
            while let Ok(join) = joins.try_recv() {
                self.join(join);
            }
            let mut wake = Instant::now() + Duration::from_millis(20);
            for at in 0..self.peers.len() {
                if let Some(timeout) = self.drain(at) {
                    wake = wake.min(timeout);
                }
            }
            let wait = wake.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            self.poll.poll(&mut events, Some(wait)).unwrap();
            for event in &events {
                match event.token() {
                    UDP => self.read_udp(),
                    LISTENER => self.accept(),
                    token => self.read_tcp(token),
                }
            }
            let now = Instant::now();
            for peer in &mut self.peers {
                let _ = peer.rtc.handle_input(Input::Timeout(now));
            }
        }
    }

    fn join(&mut self, join: Join) {
        // The same H.264 the client offers. A video m-line with no payload type is an answer
        // mediad refuses, and the call never starts. This fake still forwards only Opus.
        let mut config = RtcConfig::new().set_ice_lite(true).clear_codecs().enable_opus(true, false);
        config.codec_config().add_h264(108.into(), Some(109.into()), true, 0x42e01f);
        let mut rtc = config.build(Instant::now());
        rtc.add_local_candidate(Candidate::host(self.addr, "udp").unwrap());
        rtc.add_local_candidate(Candidate::builder().tcp().host(self.addr).tcptype(TcpType::Passive).build().unwrap());
        let offer = SdpOffer::from_sdp_string(&join.offer).unwrap();
        let answer = rtc.sdp_api().accept_offer(offer).unwrap().to_sdp_string();
        // The answer's m-lines, from our side: we receive their microphone and send on each of
        // their slots.
        let (mut publish, mut slots) = (None, Vec::new());
        for section in answer.split("m=audio").skip(1) {
            let mid = section.lines().find_map(|line| line.strip_prefix("a=mid:")).unwrap().trim();
            if section.contains("a=recvonly") && publish.is_none() {
                publish = Some(Mid::from(mid));
            } else if section.contains("a=sendonly") {
                slots.push(Mid::from(mid));
            }
        }
        let named = slots.iter().map(ToString::to_string).collect();
        self.peers.push(Peer { name: join.name, rtc, publish, slots, connected: false });
        let at = self.peers.len() - 1;
        self.drain(at);
        let _ = join.answer.send((answer, named));
    }

    fn drain(&mut self, at: usize) -> Option<Instant> {
        let mut forward = Vec::new();
        let timeout = loop {
            let peer = &mut self.peers[at];
            if !peer.rtc.is_alive() {
                break None;
            }
            match peer.rtc.poll_output() {
                Ok(Output::Timeout(timeout)) => break Some(timeout),
                Ok(Output::Transmit(transmit)) => match transmit.proto {
                    Protocol::Udp => drop(self.udp.send_to(&transmit.contents, transmit.destination)),
                    _ => {
                        let stream = self.streams.values_mut().find(|(client, _, _)| *client == transmit.destination);
                        if let Some((_, conn, _)) = stream {
                            let _ = conn.send(&transmit.contents);
                        }
                    }
                },
                Ok(Output::Event(Event::Connected)) => peer.connected = true,
                Ok(Output::Event(Event::MediaData(data))) if Some(data.mid) == peer.publish => {
                    let mut seen = self.seen.lock().unwrap();
                    *seen.frames.entry(peer.name.clone()).or_default() += 1;
                    if data.data.ends_with(&[0xfa, 0xfa]) {
                        *seen.sealed.entry(peer.name.clone()).or_default() += 1;
                    }
                    drop(seen);
                    forward.push(data);
                }
                Ok(_) => {}
                Err(_) => break None,
            }
        };
        for data in forward {
            self.forward(at, &data);
        }
        timeout
    }

    /// One frame from `from` to everyone else, on the slot each keeps for them.
    fn forward(&mut self, from: usize, data: &MediaData) {
        for to in 0..self.peers.len() {
            if to == from || !self.peers[to].connected {
                continue;
            }
            // Each listener's slots go to the others in the order they joined.
            let slot = if from < to { from } else { from - 1 };
            let peer = &mut self.peers[to];
            let Some(&mid) = peer.slots.get(slot) else {
                continue;
            };
            let Some(writer) = peer.rtc.writer(mid) else {
                continue;
            };
            let Some(pt) = writer.match_params(data.params) else {
                continue;
            };
            let _ = writer.write(pt, data.network_time, data.time, data.data.clone());
            self.drain(to);
        }
    }

    /// Hands a packet to whichever peer's ICE credentials it carries, then drains it.
    fn input(&mut self, proto: Protocol, source: SocketAddr, packet: &[u8], known: Option<usize>) -> Option<usize> {
        let contents = packet.try_into().ok()?;
        let input = Input::Receive(Instant::now(), Receive { proto, source, destination: self.addr, contents });
        let at = known.filter(|&at| self.peers[at].rtc.accepts(&input)).or_else(|| self.peers.iter().position(|peer| peer.rtc.accepts(&input)))?;
        // RFC 7983: 128 to 191 is RTP or RTCP.
        if packet.first().is_some_and(|byte| (128..192).contains(byte)) {
            let mut seen = self.seen.lock().unwrap();
            let over = seen.over.entry(self.peers[at].name.clone()).or_default();
            if !over.contains(&proto) {
                over.push(proto);
            }
        }
        let _ = self.peers[at].rtc.handle_input(input);
        self.drain(at);
        Some(at)
    }

    fn read_udp(&mut self) {
        let mut buf = [0; 2000];
        while let Ok((n, source)) = self.udp.recv_from(&mut buf) {
            let packet = buf[..n].to_vec();
            self.input(Protocol::Udp, source, &packet, None);
        }
    }

    fn accept(&mut self) {
        while let Ok((mut socket, client)) = self.listener.accept() {
            let token = Token(self.next);
            self.next += 1;
            self.poll.registry().register(&mut socket, token, Interest::READABLE | Interest::WRITABLE).unwrap();
            self.streams.insert(token, (client, Conn::new(socket), None));
        }
    }

    fn read_tcp(&mut self, token: Token) {
        let Some((client, conn, _)) = self.streams.get_mut(&token) else {
            return;
        };
        let client = *client;
        let _ = conn.flush();
        let mut frames = Vec::new();
        let open = conn.read(|frame| frames.push(frame));
        for frame in frames {
            let known = self.streams.get(&token).and_then(|(_, _, peer)| *peer);
            let peer = self.input(Protocol::Tcp, client, &frame, known);
            if let Some((_, _, held)) = self.streams.get_mut(&token) {
                *held = held.or(peer);
            }
        }
        if !open {
            self.streams.remove(&token);
        }
    }
}

// ---- mediad, as chatd runs it ----

struct Mediad {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<MediaResponse>,
    seen: Vec<MediaResponse>,
}

impl Mediad {
    fn start(ice: &str, sink: &PathBuf) -> Mediad {
        Mediad::saying("tone", ice, sink)
    }

    /// A mediad whose microphone is `audio` (`TINYCLIENT_MEDIAD_AUDIO`).
    fn saying(audio: &str, ice: &str, sink: &PathBuf) -> Mediad {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mediad"))
            .arg("1")
            .env("TINYCLIENT_MEDIAD_AUDIO", audio)
            .env("TINYCLIENT_MEDIAD_SINK", sink)
            .env("TINYCLIENT_MEDIAD_ICE", ice)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (found, frames) = channel();
        std::thread::spawn(move || {
            while let Ok(frame) = read_frame::<MediaResponse>(&mut stdout) {
                if found.send(frame).is_err() {
                    break;
                }
            }
        });
        let mediad = Mediad { child, stdin, frames, seen: Vec::new() };
        let ready = mediad.frames.recv_timeout(PATIENCE).unwrap();
        assert!(matches!(ready, MediaResponse::Ready { .. }), "{ready:?}");
        mediad
    }

    fn ask(&mut self, request: MediaRequest) -> MediaResponse {
        write_frame(self.stdin.as_mut().unwrap(), &request).unwrap();
        loop {
            let frame = self.frames.recv_timeout(PATIENCE).expect("mediad did not answer");
            if frame.is_reply() {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    fn offer(&mut self, user_id: &str) -> String {
        self.offer_with(user_id, None)
    }

    fn offer_with(&mut self, user_id: &str, e2ee: Option<TinyE2ee>) -> String {
        let spec = TinyOffer { user_id: user_id.into(), host: "127.0.0.1".into(), slots: 11, muted: false, deafened: false, e2ee };
        match self.ask(MediaRequest::TinyOffer(spec)) {
            MediaResponse::Offer { sdp } => sdp,
            other => panic!("no offer: {other:?}"),
        }
    }

    fn event(&mut self, what: &str, matches: impl Fn(&MediaResponse) -> bool) -> MediaResponse {
        if let Some(at) = self.seen.iter().position(&matches) {
            return self.seen.remove(at);
        }
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.checked_duration_since(Instant::now());
            let Some(frame) = left.and_then(|left| self.frames.recv_timeout(left).ok()) else {
                panic!("no {what} event; saw {:?}", self.seen);
            };
            if matches(&frame) {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    /// Hangs up as chatd does, by closing stdin, and waits for the process to go.
    fn hang_up(mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) {
            assert!(Instant::now() < deadline, "mediad did not exit after the hang-up");
            sleep(Duration::from_millis(10));
        }
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Mediad {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn sink(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tc-tiny-{}-{name}.pcm", std::process::id()))
}

/// The level of the last `seconds` of what a mediad played. The tone is about 5657, and two
/// tones of different pitches about 8000.
fn played_level(sink: &PathBuf, seconds: f64) -> f64 {
    let bytes = std::fs::read(sink).unwrap_or_default();
    let pcm: Vec<i16> = bytes.as_chunks::<2>().0.iter().map(|&pair| i16::from_le_bytes(pair)).collect();
    let tail = &pcm[pcm.len().saturating_sub((96_000. * seconds) as usize)..];
    (tail.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / tail.len().max(1) as f64).sqrt()
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        sleep(Duration::from_millis(50));
    }
}

/// Ada and Bea join, each through the fake, and each hears the other's tone.
fn call(ice: &str, over: Protocol) {
    let sfu = Sfu::start();
    let (ada_sink, bea_sink) = (sink(&format!("{ice}-ada")), sink(&format!("{ice}-bea")));
    let mut ada = Mediad::start(ice, &ada_sink);
    let mut bea = Mediad::start(ice, &bea_sink);
    let mut slots = Vec::new();
    for (who, mediad) in [("ada", &mut ada), ("bea", &mut bea)] {
        let offer = mediad.offer(who);
        let (answer, mids) = sfu.join(who, &offer);
        assert_eq!(mids.len(), 11, "one receive slot for each other seat");
        assert_eq!(mediad.ask(MediaRequest::TinyAnswer { sdp: answer }), MediaResponse::Done);
        slots.push(mids);
    }
    // As the server would say: each one's first slot carries the other.
    let map = |mid: &str, user: &str| vec![TinySlot { mid: mid.into(), user_id: user.into() }];
    assert_eq!(ada.ask(MediaRequest::TinySlots { audio: map(&slots[0][0], "bea") }), MediaResponse::Done);
    assert_eq!(bea.ask(MediaRequest::TinySlots { audio: map(&slots[1][0], "ada") }), MediaResponse::Done);
    for mediad in [&mut ada, &mut bea] {
        mediad.event("Connected", |event| matches!(event, MediaResponse::Connected { .. }));
    }
    wait_for("both are heard by the SFU", || sfu.frames("ada") > 50 && sfu.frames("bea") > 50);
    assert_eq!(sfu.over("ada"), [over]);
    assert_eq!(sfu.over("bea"), [over]);
    wait_for("each plays the other's tone", || (4000. ..7000.).contains(&played_level(&ada_sink, 0.5)) && (4000. ..7000.).contains(&played_level(&bea_sink, 0.5)));
    // Their speaking is reported by user id: their own, and the other's from the slot map.
    ada.event("Ada speaking", |event| matches!(event, MediaResponse::Speaking { user_id, speaking: true } if user_id == "ada"));
    ada.event("Bea speaking", |event| matches!(event, MediaResponse::Speaking { user_id, speaking: true } if user_id == "bea"));

    // Muted, Ada sends nothing at all, and Bea's speaker goes quiet.
    assert_eq!(ada.ask(MediaRequest::Audio { muted: true, deafened: false }), MediaResponse::Done);
    sleep(Duration::from_millis(300));
    let muted_at = sfu.frames("ada");
    sleep(Duration::from_millis(600));
    assert_eq!(sfu.frames("ada"), muted_at, "a muted microphone sends no frames");
    wait_for("Bea hears silence", || played_level(&bea_sink, 0.2) < 50.);
    // Bea still talks to Ada.
    assert!((4000. ..7000.).contains(&played_level(&ada_sink, 0.5)));

    ada.hang_up();
    bea.hang_up();
    let _ = std::fs::remove_file(&ada_sink);
    let _ = std::fs::remove_file(&bea_sink);
}

#[test]
fn two_people_hear_each_other_over_udp() {
    call("udp", Protocol::Udp);
}

#[test]
fn two_people_hear_each_other_over_ice_tcp_alone() {
    call("tcp", Protocol::Tcp);
}

/// Both kinds offered, as a real call does: UDP carries it where it can.
#[test]
fn with_both_offered_udp_carries_the_call() {
    call("both", Protocol::Udp);
}

#[test]
fn an_answer_with_no_offer_out_and_a_bad_answer_are_refused_in_words() {
    let sink = sink("refused");
    let mut mediad = Mediad::start("udp", &sink);
    let refused = |reply: MediaResponse| matches!(reply, MediaResponse::Refused { detail } if !detail.is_empty());
    assert!(refused(mediad.ask(MediaRequest::TinyAnswer { sdp: "v=0".into() })));
    mediad.offer("ada");
    assert!(refused(mediad.ask(MediaRequest::TinyAnswer { sdp: "not an answer".into() })));
    mediad.hang_up();
}

// ---- the voice controls ----

/// Everyone in `names` joins one room through `sfu`, over UDP, each told the slot map the fake
/// keeps (each listener's slots carry the others in the order they joined), and each connected.
fn room(sfu: &Sfu, tag: &str, names: &[&str]) -> (Vec<Mediad>, Vec<PathBuf>) {
    let sinks: Vec<PathBuf> = names.iter().map(|name| sink(&format!("{tag}-{name}"))).collect();
    let mut people: Vec<Mediad> = sinks.iter().map(|sink| Mediad::start("udp", sink)).collect();
    let mut slots = Vec::new();
    for (name, mediad) in names.iter().zip(&mut people) {
        let offer = mediad.offer(name);
        let (answer, mids) = sfu.join(name, &offer);
        assert_eq!(mediad.ask(MediaRequest::TinyAnswer { sdp: answer }), MediaResponse::Done);
        slots.push(mids);
    }
    for (at, mediad) in people.iter_mut().enumerate() {
        let others = names.iter().enumerate().filter(|(other, _)| *other != at).map(|(_, name)| *name);
        let map = others.zip(&slots[at]).map(|(user, mid)| TinySlot { mid: mid.clone(), user_id: user.into() }).collect();
        assert_eq!(mediad.ask(MediaRequest::TinySlots { audio: map }), MediaResponse::Done);
        mediad.event("Connected", |event| matches!(event, MediaResponse::Connected { .. }));
    }
    (people, sinks)
}

fn done(mediad: &mut Mediad, request: MediaRequest) {
    assert_eq!(mediad.ask(request), MediaResponse::Done);
}

fn leave(people: Vec<Mediad>, sinks: Vec<PathBuf>) {
    for mediad in people {
        mediad.hang_up();
    }
    for sink in sinks {
        let _ = std::fs::remove_file(sink);
    }
}

/// Whether `name`'s frames stop reaching the SFU: none in the half second after a pause long
/// enough for a release delay and the trail of silence after it.
fn stays_silent(sfu: &Sfu, name: &str) -> bool {
    sleep(Duration::from_millis(700));
    let before = sfu.frames(name);
    sleep(Duration::from_millis(500));
    sfu.frames(name) == before
}

fn keeps_talking(sfu: &Sfu, name: &str) -> bool {
    let before = sfu.frames(name);
    sleep(Duration::from_millis(500));
    sfu.frames(name) > before + 15
}

/// V9: one person's volume for me, and muting them for me alone. The others hear them as before.
#[test]
fn each_voice_plays_at_the_volume_chosen_for_it_and_a_muted_one_not_at_all() {
    let sfu = Sfu::start();
    let (mut people, sinks) = room(&sfu, "gain", &["ada", "bea"]);
    wait_for("Ada hears Bea's tone", || (4000. ..7000.).contains(&played_level(&sinks[0], 0.5)));
    done(&mut people[0], MediaRequest::Gain { user_id: "bea".into(), percent: 50, muted: false });
    wait_for("at 50 % Bea is half as loud", || (2200. ..3500.).contains(&played_level(&sinks[0], 0.4)));
    done(&mut people[0], MediaRequest::Gain { user_id: "bea".into(), percent: 200, muted: false });
    wait_for("at 200 % twice as loud", || (8800. ..14_000.).contains(&played_level(&sinks[0], 0.4)));
    done(&mut people[0], MediaRequest::Gain { user_id: "bea".into(), percent: 200, muted: true });
    wait_for("muted for Ada, Bea is not played", || played_level(&sinks[0], 0.3) < 1.);
    // Bea still speaks, Ada still sees it, and Bea still hears Ada.
    people[0].event("Bea speaking", |event| matches!(event, MediaResponse::Speaking { user_id, speaking: true } if user_id == "bea"));
    assert!((4000. ..7000.).contains(&played_level(&sinks[1], 0.5)));
    done(&mut people[0], MediaRequest::Gain { user_id: "bea".into(), percent: 100, muted: false });
    wait_for("back to as decoded", || (4000. ..7000.).contains(&played_level(&sinks[0], 0.4)));
    leave(people, sinks);
}

/// While someone holding priority speech talks, everyone else is 12 dB quieter for the listener.
/// Ada mutes Cy for herself so that only Bea is played, and Cy is still heard speaking.
#[test]
fn a_priority_speaker_ducks_everyone_else_by_twelve_db() {
    let sfu = Sfu::start();
    let (mut people, sinks) = room(&sfu, "duck", &["ada", "bea", "cy"]);
    done(&mut people[0], MediaRequest::Gain { user_id: "cy".into(), percent: 100, muted: true });
    wait_for("Ada hears Bea alone", || (4000. ..7000.).contains(&played_level(&sinks[0], 0.5)));
    sleep(Duration::from_millis(500));
    let alone = played_level(&sinks[0], 0.3);
    done(&mut people[0], MediaRequest::Priority { user_ids: vec!["cy".into()] });
    wait_for("Bea is ducked under Cy", || played_level(&sinks[0], 0.3) < alone / 2.);
    sleep(Duration::from_millis(300));
    let ducked = played_level(&sinks[0], 0.3);
    let db = 20. * (ducked / alone).log10();
    assert!((-13.5..-10.5).contains(&db), "ducked by {db:.1} dB");
    // Bea's own priority is no reason to duck Bea.
    done(&mut people[0], MediaRequest::Priority { user_ids: vec!["bea".into()] });
    wait_for("Bea is back", || played_level(&sinks[0], 0.3) > alone * 0.8);
    leave(people, sinks);
}

/// V8: push-to-talk sends only while the key is held and its release delay after; voice
/// activation sends from its threshold up. The tone's level is about 74.
#[test]
fn push_to_talk_and_the_voice_threshold_decide_what_is_sent() {
    let sfu = Sfu::start();
    let (mut people, sinks) = room(&sfu, "ptt", &["ada", "bea"]);
    let ada = &mut people[0];
    assert!(keeps_talking(&sfu, "ada"), "voice activation at the default sends the tone");
    done(ada, MediaRequest::Input(InputMode::Voice { threshold: 90 }));
    assert!(stays_silent(&sfu, "ada"), "under a threshold of 90 nothing is sent");
    done(ada, MediaRequest::Input(InputMode::Voice { threshold: 60 }));
    assert!(keeps_talking(&sfu, "ada"));

    done(ada, MediaRequest::Input(InputMode::PushToTalk { release_ms: 300 }));
    assert!(stays_silent(&sfu, "ada"), "push-to-talk with no key held sends nothing");
    ada.event("Ada stopped", |event| matches!(event, MediaResponse::Speaking { user_id, speaking: false } if user_id == "ada"));
    ada.seen.clear();
    done(ada, MediaRequest::Talk { held: true, priority: false });
    assert!(keeps_talking(&sfu, "ada"), "held, it sends");
    ada.event("Ada speaking", |event| matches!(event, MediaResponse::Speaking { user_id, speaking: true } if user_id == "ada"));
    ada.seen.clear();
    done(ada, MediaRequest::Talk { held: false, priority: false });
    let let_go = Instant::now();
    ada.event("Ada stopped", |event| matches!(event, MediaResponse::Speaking { user_id, speaking: false } if user_id == "ada"));
    let after = let_go.elapsed();
    assert!(after >= Duration::from_millis(250) && after < Duration::from_millis(800), "stopped {after:?} after the key");
    assert!(stays_silent(&sfu, "ada"));
    // The priority key opens it too.
    done(ada, MediaRequest::Talk { held: true, priority: true });
    assert!(keeps_talking(&sfu, "ada"));
    done(ada, MediaRequest::Talk { held: false, priority: true });
    assert!(stays_silent(&sfu, "ada"));
    leave(people, sinks);
}

/// Stats once media flows: the path, the codec, both rates, and a bitrate that follows the
/// room's.
#[test]
fn the_connection_reports_itself_and_the_bitrate_follows_the_room() {
    let sfu = Sfu::start();
    let (mut people, sinks) = room(&sfu, "stats", &["ada", "bea"]);
    let stats = |mediad: &mut Mediad| match mediad.ask(MediaRequest::Stats) {
        MediaResponse::Stats(stats) => Some(stats),
        _ => None,
    };
    let ada = &mut people[0];
    let deadline = Instant::now() + PATIENCE;
    while !stats(ada).is_some_and(|now| now.send_kbps > 0 && now.receive_kbps > 0) {
        assert!(Instant::now() < deadline, "Ada never measured a second both ways");
        sleep(Duration::from_millis(100));
    }
    sleep(Duration::from_millis(1100));
    let before = stats(ada).unwrap();
    assert_eq!((before.path, before.codec.as_str()), (CallPath::Udp, "opus"), "{before:?}");
    assert!(before.loss_permille < 50 && before.jitter_ms < 40, "{before:?}");
    done(ada, MediaRequest::Bitrate { kbps: 16 });
    // One published second can straddle the change. Wait until a later one is entirely at
    // the room's rate, which a slow runner may not have by a fixed pause.
    let deadline = Instant::now() + PATIENCE;
    loop {
        sleep(Duration::from_millis(200));
        let now = stats(ada).unwrap();
        if now.send_kbps + 8 < before.send_kbps {
            break;
        }
        assert!(Instant::now() < deadline, "16 kb/s sends less: {} then {}", before.send_kbps, now.send_kbps);
    }
    leave(people, sinks);
}

/// The cue for being put out of the call plays out after the hang-up, before the devices go.
#[test]
fn the_disconnected_cue_is_heard_after_the_hang_up() {
    let sfu = Sfu::start();
    let (mut people, sinks) = room(&sfu, "cue", &["ada", "bea"]);
    done(&mut people[0], MediaRequest::Audio { muted: false, deafened: true });
    wait_for("deafened, Ada plays nothing", || played_level(&sinks[0], 0.3) < 1.);
    let mut ada = people.remove(0);
    done(&mut ada, MediaRequest::Cue { cue: Cue::Disconnected });
    ada.hang_up();
    let level = played_level(&sinks[0], 0.3);
    assert!(level > 100., "the last of what Ada played is the cue: {level}");
    leave(people, sinks);
}

// ---- end-to-end encryption ----

const ROOM: &str = "e2ee-room";

/// The server's side of `call.e2ee`, as tinyserver plays it: the external sender, proposals at
/// the group's epoch to everyone in the group, and the first commit that comes back wins.
struct Relay {
    mls: mls::Server,
    epoch: u64,
}

fn e2ee_frame(op: &str, fields: serde_json::Value) -> MediaRequest {
    let mut frame = fields;
    frame["type"] = "call.e2ee".into();
    frame["room_id"] = ROOM.into();
    frame["op"] = op.into();
    MediaRequest::TinyE2ee { frame: frame.to_string() }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(frame: &serde_json::Value, field: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(frame[field].as_str().unwrap()).unwrap()
}

impl Mediad {
    /// The next `call.e2ee` frame this mediad has for the server, with `op`.
    fn said(&mut self, op: &str) -> serde_json::Value {
        let wanted = format!("\"op\":\"{op}\"");
        let MediaResponse::TinyE2ee { frame } = self.event(op, |event| matches!(event, MediaResponse::TinyE2ee { frame } if frame.contains(&wanted))) else {
            unreachable!()
        };
        serde_json::from_str(&frame).unwrap()
    }

    /// The protection mediad says the call has, once `done` holds of it.
    fn security(&mut self, what: &str, done: impl Fn(&protocol::CallSecurity) -> bool) -> protocol::CallSecurity {
        let MediaResponse::TinySecurity(security) = self.event(what, |event| matches!(event, MediaResponse::TinySecurity(security) if done(security))) else {
            unreachable!()
        };
        security
    }
}

impl Relay {
    fn start(&self, mediad: &mut Mediad) -> Vec<u8> {
        done(mediad, e2ee_frame("start", serde_json::json!({"version": 1, "external_sender": b64(&self.mls.package())})));
        unb64(&mediad.said("key_package"), "data")
    }

    /// One batch: proposals to every member, the first member's commit to them all, and its
    /// welcome to whoever it adds.
    fn batch(&mut self, members: &mut [&mut Mediad], messages: &[Vec<u8>], added: &mut [&mut Mediad]) {
        let proposals = e2ee_frame("proposals", serde_json::json!({"epoch": self.epoch, "data": b64(&mls::append(messages))}));
        let mut commits = Vec::new();
        for member in members.iter_mut() {
            done(member, proposals.clone());
            commits.push(member.said("commit"));
        }
        let won = &commits[0];
        for member in members.iter_mut() {
            done(member, e2ee_frame("commit", serde_json::json!({"epoch": self.epoch, "data": won["data"]})));
        }
        self.epoch += 1;
        for joiner in added.iter_mut() {
            done(joiner, e2ee_frame("welcome", serde_json::json!({"epoch": self.epoch, "data": won["welcome"]})));
        }
    }
}

/// Ada and Bea, then Cy, in a room whose call is end-to-end encrypted. The fake SFU forwards
/// only sealed frames and still everyone hears everyone; every join and leave moves the epoch
/// and the call code, and everyone in the call sees the same code.
#[test]
fn an_end_to_end_encrypted_room_is_heard_through_frames_the_sfu_cannot_read() {
    let sfu = Sfu::start();
    let mut relay = Relay { mls: mls::Server::new(), epoch: 0 };
    let group = mediad::e2ee::group_id(ROOM);
    let names = ["ada", "bea", "cy"];
    let sinks: Vec<PathBuf> = names.iter().map(|name| sink(&format!("e2ee-{name}"))).collect();
    let keys = std::env::temp_dir().join(format!("tc-tiny-e2ee-{}", std::process::id()));
    // Each says a tone of its own: two of one pitch, a whole number of 20 ms frames apart, can
    // all but cancel in the mix of whoever hears both.
    let tones = ["tone:440", "tone:550", "tone:660"];
    let mut people: Vec<Mediad> = sinks.iter().zip(tones).map(|(sink, tone)| Mediad::saying(tone, "udp", sink)).collect();
    let mut slots = Vec::new();
    for (name, mediad) in names.iter().zip(&mut people) {
        let e2ee = TinyE2ee { room_id: ROOM.into(), identity: keys.join(name).to_string_lossy().into_owned(), required: false };
        let offer = mediad.offer_with(name, Some(e2ee));
        mediad.security("pending until the server says", |security| security.mode == SecurityMode::Pending);
        let (answer, mids) = sfu.join(name, &offer);
        done(mediad, MediaRequest::TinyAnswer { sdp: answer });
        slots.push(mids);
    }
    let roster = |count: usize| MediaRequest::TinyRoster { user_ids: names[..count].iter().map(|name| name.to_string()).collect() };
    let map = |at: usize, count: usize| {
        let others = names[..count].iter().enumerate().filter(|(other, _)| *other != at).map(|(_, name)| *name);
        MediaRequest::TinySlots { audio: others.zip(&slots[at]).map(|(user, mid)| TinySlot { mid: mid.clone(), user_id: user.into() }).collect() }
    };
    let (ada, rest) = people.split_first_mut().unwrap();
    let (bea, rest) = rest.split_first_mut().unwrap();
    let cy = &mut rest[0];

    // Ada makes the group; the server proposes Bea to her.
    for mediad in [&mut *ada, &mut *bea] {
        done(mediad, roster(2));
    }
    relay.start(ada);
    let bea_package = relay.start(bea);
    let add_bea = relay.mls.add_message(&bea_package, group, 0);
    relay.batch(&mut [&mut *ada], &[add_bea], &mut [&mut *bea]);
    let two = ada.security("end to end", |security| security.mode == SecurityMode::EndToEnd && security.epoch == 1);
    assert_eq!(bea.security("end to end", |security| security.mode == SecurityMode::EndToEnd).code, two.code);
    assert_eq!(two.code.len(), 30);
    assert_eq!(two.members.len(), 1);
    assert_eq!(two.members[0].user_id, "bea");
    for (at, mediad) in [(0, &mut *ada), (1, &mut *bea)] {
        done(mediad, map(at, 2));
        mediad.event("Connected", |event| matches!(event, MediaResponse::Connected { .. }));
    }
    wait_for("each plays the other's tone", || (4000. ..7000.).contains(&played_level(&sinks[0], 0.5)) && (4000. ..7000.).contains(&played_level(&sinks[1], 0.5)));
    wait_for("the SFU forwarded plenty", || sfu.frames("ada") > 50 && sfu.frames("bea") > 50);
    for name in ["ada", "bea"] {
        // Only silence, which DAVE leaves clear, may pass unsealed.
        assert!(sfu.sealed(name) + 5 >= sfu.frames(name), "{name}: {} of {} sealed", sfu.sealed(name), sfu.frames(name));
    }

    // Cy joins: everyone in the group gets the proposal, Cy the welcome, and there is a new code.
    for mediad in [&mut *ada, &mut *bea, &mut *cy] {
        done(mediad, roster(3));
    }
    let cy_package = relay.start(cy);
    let add_cy = relay.mls.add_message(&cy_package, group, 1);
    relay.batch(&mut [&mut *ada, &mut *bea], &[add_cy], &mut [&mut *cy]);
    let three = ada.security("Cy in", |security| security.epoch == 2);
    assert_eq!(bea.security("Cy in", |security| security.epoch == 2).code, three.code);
    assert_eq!(cy.security("Cy in", |security| security.epoch == 2).code, three.code);
    assert_ne!(three.code, two.code, "a join is a new epoch and a new code");
    for (at, mediad) in [(0, &mut *ada), (1, &mut *bea), (2, &mut *cy)] {
        done(mediad, map(at, 3));
    }
    cy.event("Connected", |event| matches!(event, MediaResponse::Connected { .. }));
    // One tone alone is about 5657; both, at their two pitches, about 8000.
    wait_for("Cy hears both the others", || played_level(&sinks[2], 0.5) > 7000.);
    // Cy leaves the group (leaf 2) and the call's map, though its mediad still runs and the
    // SFU still sends it what the others say: the rest move on with keys Cy never gets. (A
    // hung-up mediad's port answers the fake's UDP with resets Windows reports on the fake's
    // own socket, so Cy stays up until the end.)
    for mediad in [&mut *ada, &mut *bea] {
        done(mediad, roster(2));
    }
    let remove_cy = relay.mls.remove_message(2, group, 2);
    relay.batch(&mut [&mut *ada, &mut *bea], &[remove_cy], &mut []);
    let after = ada.security("Cy out", |security| security.epoch == 3);
    assert_eq!(bea.security("Cy out", |security| security.epoch == 3).code, after.code);
    assert_ne!(after.code, three.code);
    assert_eq!(after.members.len(), 1, "Cy is no longer in the group");
    done(ada, map(0, 2));
    done(bea, map(1, 2));
    wait_for("Cy hears nothing said after it left", || played_level(&sinks[2], 0.5) < 50.);
    wait_for("Ada still hears Bea alone", || (4000. ..7000.).contains(&played_level(&sinks[0], 0.5)));
    wait_for("Bea still hears Ada alone", || (4000. ..7000.).contains(&played_level(&sinks[1], 0.5)));
    leave(people, sinks);
    let _ = std::fs::remove_dir_all(keys);
}

/// A room the server says is transport-only sends and plays in clear, and says so.
#[test]
fn a_transport_only_room_says_so_and_is_heard_in_clear() {
    let sfu = Sfu::start();
    let names = ["ada", "bea"];
    let sinks: Vec<PathBuf> = names.iter().map(|name| sink(&format!("off-{name}"))).collect();
    let mut people: Vec<Mediad> = sinks.iter().map(|sink| Mediad::start("udp", sink)).collect();
    let mut slots = Vec::new();
    let keys = std::env::temp_dir().join(format!("tc-tiny-off-{}", std::process::id()));
    for (name, mediad) in names.iter().zip(&mut people) {
        // Ada's app encrypts end to end; Bea's server, as Bea's app sees it, never offered it.
        let e2ee = (*name == "ada").then(|| TinyE2ee { room_id: ROOM.into(), identity: keys.join(name).to_string_lossy().into_owned(), required: false });
        let offer = mediad.offer_with(name, e2ee);
        let (answer, mids) = sfu.join(name, &offer);
        done(mediad, MediaRequest::TinyAnswer { sdp: answer });
        slots.push(mids);
    }
    done(&mut people[0], e2ee_frame("off", serde_json::json!({"user_ids": ["bea"]})));
    let ada = people[0].security("transport only", |security| security.mode == SecurityMode::Transport);
    assert_eq!(ada.unsupported, ["bea"]);
    assert!(!ada.server_lacks && ada.code.is_empty());
    people[1].security("no end-to-end encryption", |security| security.mode == SecurityMode::Transport && security.server_lacks);
    for (at, mediad) in people.iter_mut().enumerate() {
        let other = names[1 - at];
        done(mediad, MediaRequest::TinySlots { audio: vec![TinySlot { mid: slots[at][0].clone(), user_id: other.into() }] });
        mediad.event("Connected", |event| matches!(event, MediaResponse::Connected { .. }));
    }
    wait_for("each plays the other's tone", || (4000. ..7000.).contains(&played_level(&sinks[0], 0.5)) && (4000. ..7000.).contains(&played_level(&sinks[1], 0.5)));
    assert_eq!(sfu.sealed("ada"), 0, "nothing is sealed in a transport-only call");
    leave(people, sinks);
    let _ = std::fs::remove_dir_all(keys);
}
