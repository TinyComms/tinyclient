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

use mediad::framing::Conn;
use mio::net::{TcpListener, UdpSocket};
use mio::{Events, Interest, Poll, Token};
use protocol::{read_frame, write_frame, MediaRequest, MediaResponse, TinyOffer, TinySlot};
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
        let mut udp = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = udp.local_addr().unwrap();
        let mut listener = TcpListener::bind(addr).unwrap();
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
        let mut rtc = RtcConfig::new().set_ice_lite(true).clear_codecs().enable_opus(true, false).build(Instant::now());
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
                    *self.seen.lock().unwrap().frames.entry(peer.name.clone()).or_default() += 1;
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_mediad"))
            .arg("1")
            .env("TINYCLIENT_MEDIAD_AUDIO", "tone")
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
        let spec = TinyOffer { user_id: user_id.into(), host: "127.0.0.1".into(), slots: 11, muted: false, deafened: false };
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

/// The level of the last `seconds` of what a mediad played. The tone is about 5657.
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
