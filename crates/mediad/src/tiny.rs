//! A call on a Tiny server: WebRTC to the server's SFU through str0m, a sans-IO library this
//! process drives from its own loop. tinyserver's `docs/calls.md` is the contract, and its
//! headless client (`crates/sfu/src/client.rs`) is what this follows.
//!
//! 1. `TinyOffer` makes the offer ([`Offer::new`]): one UDP socket, a host candidate for it and
//!    an active ICE-TCP candidate (RFC 6544) on the same address, then one `sendonly` audio
//!    m-line for the microphone and one `recvonly` per receive slot. Opus only. We are the full
//!    ICE agent and the server is ICE-lite; no trickle and no STUN server, since the server
//!    learns our address from our checks. A UDP pair outranks a TCP one, so TCP carries the call
//!    only where UDP cannot get through, as on a network that blocks it.
//! 2. chatd sends the offer as `call.join`, and hands back the answer ([`Offer::accept`]) and
//!    every `call.slots`.
//! 3. [`run`] is the call: a 20 ms tick as in `media`, with str0m's timers and packets in
//!    between. The microphone goes through the same speech gate, as mono Opus with each frame's
//!    RFC 6464 level, stamped n × 960 at 48 kHz. Muted, nothing is sent. Each frame that comes
//!    in goes to its slot's jitter buffer and decoder; a slot that changes hands starts both
//!    afresh, since the new speaker's RTP times are not the last one's.
//!
//! The devices open only once media flows, so a join that fails never lights the microphone
//! indicator. The microphone goes through the same processing as a Discord call (`process`):
//! the mix played is the echo canceller's reference. Each voice is played at the volume the
//! user chose for that person, ducked under a priority speaker (`hear`); the encoder runs at the
//! room's bitrate; and str0m's statistics, once a second, give the round trip and the path for
//! `Stats`.

use crate::audio::{level, Audio};
use crate::cue::Playing;
use crate::framing::Conn;
use crate::hear::{Ear, Limiter};
use crate::media::{self, announce, finish_cue, listen, mixdown, Devices, Gate, Send, Stream, MICROPHONE_DENIED};
use crate::process::Processor;
use crate::stats::Meter;
use crate::opus::Encoder;
use crate::permission::{microphone, Microphone};
use crate::rtp::SILENCE;
use crate::session::Controls;
use crate::{out, rt, FRAME, FRAME_SAMPLES};
use mio::net::{TcpStream, UdpSocket};
use mio::{Events, Interest, Poll, Token};
use protocol::{CallFailure, CallPath, Cue, MediaResponse, TinyOffer, TinySlot};
use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive, TcpType};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

const TICK: Duration = Duration::from_millis(20);
const SECOND: u64 = 50;
/// The level from which someone else's frame counts as speech, as in a Discord call.
const VOICE: u8 = 25;
const UDP: Token = Token(0);
/// The discard port, which an active TCP candidate advertises (RFC 6544).
const ACTIVE_PORT: u16 = 9;
/// Receive slots held at most, whatever a server sends.
const SLOTS: usize = 64;
/// How long media has to start once the answer is in. The server gives up at 15 s
/// (`media_timeout`); this says why first.
const CONNECT: Duration = Duration::from_secs(12);
/// How long a call that lost its media waits for it, or for a new connection, before it ends.
const COME_BACK: Duration = Duration::from_secs(30);

const NO_PATH: &str = "The server answered, but call audio couldn't get through. A VPN or firewall may be blocking it.";
const DROPPED: &str = "The connection to the server dropped and didn't come back. You've left the call.";
const ENGINE: &str = "The call engine could not start. Try again.";

/// Which candidates to offer. `TINYCLIENT_MEDIAD_ICE` makes a test offer one kind alone: `udp`,
/// or `tcp`, which is how a network that blocks UDP looks to the server.
fn transports() -> (bool, bool) {
    match std::env::var("TINYCLIENT_MEDIAD_ICE").as_deref() {
        Ok("udp") => (true, false),
        Ok("tcp") => (false, true),
        _ => (true, true),
    }
}

/// The address of the interface that reaches `host`, which is the one to offer. Asking the
/// system for a route sends nothing. Without one to the server, the default route's; without
/// that, loopback.
fn local_ip(host: &str) -> IpAddr {
    let reach = |target: SocketAddr| -> Option<IpAddr> {
        let probe = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        probe.connect(target).ok()?;
        let ip = probe.local_addr().ok()?.ip();
        (!ip.is_unspecified()).then_some(ip)
    };
    let server = (host, 443).to_socket_addrs().ok().into_iter().flatten().find(SocketAddr::is_ipv4);
    server
        .and_then(reach)
        // TEST-NET-1: only its route is asked for.
        .or_else(|| reach(SocketAddr::from(([192, 0, 2, 1], 9))))
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// One connection to the SFU: str0m's state and the sockets it speaks through.
pub struct Link {
    rtc: Rtc,
    udp: Option<UdpSocket>,
    /// One stream for each server TCP candidate str0m sends to, opened on first use.
    tcp: HashMap<SocketAddr, (Token, Conn)>,
    /// Each packet in is handed to str0m with the candidate it came to as its destination.
    local_udp: Option<SocketAddr>,
    local_tcp: Option<SocketAddr>,
    microphone: Mid,
    /// When the answer was taken.
    answered: Instant,
}

/// An offer sent, and the answer not yet in.
pub struct Offer {
    link: Link,
    pending: SdpPendingOffer,
    sdp: String,
}

impl Offer {
    pub fn new(spec: &TinyOffer) -> io::Result<Offer> {
        let ip = local_ip(&spec.host);
        let (udp, tcp) = transports();
        let mut rtc = RtcConfig::new().clear_codecs().enable_opus(true, false).set_stats_interval(Some(Duration::from_secs(1))).build(Instant::now());
        let candidate = |made: Result<Candidate, str0m::error::IceError>| made.map_err(|err| io::Error::other(format!("candidate: {err:?}")));
        let (mut socket, mut local_udp, mut local_tcp) = (None, None, None);
        if udp {
            // Every interface: the candidate names the one that reaches the server, and a
            // reply that comes back on another is still ours.
            let bound = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
            let local = SocketAddr::new(ip, bound.local_addr()?.port());
            rtc.add_local_candidate(candidate(Candidate::host(local, "udp"))?);
            (socket, local_udp) = (Some(bound), Some(local));
        }
        if tcp {
            let local = SocketAddr::new(ip, ACTIVE_PORT);
            rtc.add_local_candidate(candidate(Candidate::builder().tcp().host(local).tcptype(TcpType::Active).build())?);
            local_tcp = Some(local);
        }
        let mut change = rtc.sdp_api();
        let microphone = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        for _ in 0..spec.slots {
            change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
        }
        let (offer, pending) = change.apply().ok_or_else(|| io::Error::other("nothing to offer"))?;
        let link = Link { rtc, udp: socket, tcp: HashMap::new(), local_udp, local_tcp, microphone, answered: Instant::now() };
        Ok(Offer { link, pending, sdp: offer.to_sdp_string() })
    }

    /// The SDP for `call.join`.
    pub fn sdp(&self) -> &str {
        &self.sdp
    }

    /// Takes the SDP from `call.answer`.
    pub fn accept(mut self, answer: &str) -> Result<Link, String> {
        let answer = SdpAnswer::from_sdp_string(answer).map_err(|err| format!("{err:?}"))?;
        self.link.rtc.sdp_api().accept_answer(self.pending, answer).map_err(|err| format!("{err:?}"))?;
        self.link.answered = Instant::now();
        Ok(self.link)
    }
}

/// A packet read and not yet handed to str0m.
type Packet = (Protocol, SocketAddr, Vec<u8>);

/// Applies the server's slot map to who holds which slot now. Gives the slots whose speaker
/// changed, which start a fresh buffer and decoder, and the slots no longer held, which go.
/// A slot whose speaker was not known yet (audio before its first map) keeps what it has.
pub fn remap(held: &mut HashMap<String, String>, map: &[TinySlot]) -> (Vec<String>, Vec<String>) {
    let map: HashMap<&str, &str> = map.iter().take(SLOTS).map(|slot| (slot.mid.as_str(), slot.user_id.as_str())).collect();
    let mut freed: Vec<String> = held.keys().filter(|mid| !map.contains_key(mid.as_str())).cloned().collect();
    freed.sort();
    for mid in &freed {
        held.remove(mid);
    }
    let mut reset = Vec::new();
    for (mid, user) in map {
        match held.get(mid) {
            Some(was) if was == user => {}
            Some(was) if was.is_empty() => {}
            Some(_) => reset.push(mid.to_owned()),
            None => {}
        }
        held.insert(mid.to_owned(), user.to_owned());
    }
    reset.sort();
    (reset, freed)
}

/// One receive slot: whose voice it carries, and that voice on its way to the mix.
struct Slot {
    user: String,
    stream: Stream,
}

struct Call {
    controls: Arc<Controls>,
    user_id: String,
    link: Link,
    poll: Poll,
    next_token: usize,
    packets: VecDeque<Packet>,
    /// Media flows on this link.
    connected: bool,
    /// Lost media, and since when: the call ends if it does not come back.
    lost: Option<Instant>,
    audio: Option<Audio>,
    devices: Option<Devices>,
    microphone_unasked: bool,
    encoder: Encoder,
    gate: Gate,
    slots: HashMap<String, Slot>,
    ticks: u64,
    levels: (u8, u8),
    cue: Option<Playing>,
    processor: Processor,
    limiter: Limiter,
    meter: Meter,
}

/// The RFC 6464 level of a frame, in dBov: 0 is the loudest, -127 silence.
pub fn dbov(frame: &[i16]) -> i8 {
    let power = frame.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / frame.len().max(1) as f64;
    if power < 1. {
        return -127;
    }
    let db = 10. * (power / f64::from(i16::MAX).powi(2)).log10();
    db.round().clamp(-127., 0.) as i8
}

impl Call {
    fn register(&mut self) -> io::Result<()> {
        if let Some(udp) = &mut self.link.udp {
            self.poll.registry().register(udp, UDP, Interest::READABLE)?;
        }
        Ok(())
    }

    /// Closes a link's sockets before another takes its place.
    fn close(&mut self) {
        if let Some(udp) = &mut self.link.udp {
            let _ = self.poll.registry().deregister(udp);
        }
        for (_, (_, conn)) in self.link.tcp.drain() {
            let mut conn = conn;
            let _ = self.poll.registry().deregister(&mut conn.socket);
        }
    }

    /// The call joined again (the server restarted): a new connection, and nobody's slot is
    /// what it was.
    fn take_up(&mut self, link: Link) {
        self.close();
        self.link = link;
        if let Err(err) = self.register() {
            eprintln!("mediad: the new connection's socket: {err}");
        }
        self.packets.clear();
        if self.connected {
            self.connected = false;
            out::send(&MediaResponse::Interrupted);
        }
        self.lost.get_or_insert_with(Instant::now);
        for (_, slot) in self.slots.drain() {
            if slot.stream.speaking() && !slot.user.is_empty() {
                out::send(&MediaResponse::Speaking { user_id: slot.user, speaking: false });
            }
        }
    }

    fn apply_slots(&mut self, map: &[TinySlot]) {
        let mut held: HashMap<String, String> = self.slots.iter().map(|(mid, slot)| (mid.clone(), slot.user.clone())).collect();
        let (reset, freed) = remap(&mut held, map);
        for mid in freed.iter().chain(&reset) {
            if let Some(slot) = self.slots.remove(mid) {
                if slot.stream.speaking() && !slot.user.is_empty() {
                    out::send(&MediaResponse::Speaking { user_id: slot.user, speaking: false });
                }
            }
        }
        for (mid, user) in held {
            match self.slots.get_mut(&mid) {
                Some(slot) => slot.user = user,
                None => {
                    if let Some(stream) = Stream::new() {
                        self.slots.insert(mid, Slot { user, stream });
                    }
                }
            }
        }
    }

    /// Polls str0m until it asks for a timeout. `None` once this connection is over.
    fn drain(&mut self) -> Option<Instant> {
        loop {
            if !self.link.rtc.is_alive() {
                return None;
            }
            match self.link.rtc.poll_output().ok()? {
                Output::Timeout(at) => return Some(at),
                Output::Transmit(transmit) => self.transmit(transmit.proto, transmit.destination, &transmit.contents),
                Output::Event(event) => self.event(event),
            }
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Connected => self.up(),
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => self.down(),
            Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed) if self.lost.is_some() && self.audio.is_some() => self.up(),
            Event::PeerStats(stats) => {
                let pair = stats.selected_candidate_pair.as_ref();
                self.meter.round_trip(stats.rtt.or_else(|| pair.and_then(|pair| pair.current_round_trip_time)));
                // Only host candidates are offered: the path is UDP or ICE-TCP, never a relay.
                if let Some(pair) = pair {
                    self.meter.path = match pair.protocol {
                        Protocol::Udp => CallPath::Udp,
                        _ => CallPath::Tcp,
                    };
                }
            }
            // What the server sees of our microphone: its FEC is planned for that loss.
            Event::MediaEgressStats(stats) if stats.mid == self.link.microphone => {
                if let Some(loss) = stats.loss {
                    self.encoder.set_loss(loss);
                }
            }
            Event::MediaData(data) => {
                let mid = data.mid.to_string();
                if !self.slots.contains_key(&mid) {
                    // Audio before the map that names its speaker: heard, and named later.
                    if self.slots.len() >= SLOTS {
                        return;
                    }
                    let Some(stream) = Stream::new() else {
                        return;
                    };
                    self.slots.insert(mid.clone(), Slot { user: String::new(), stream });
                }
                let Some(slot) = self.slots.get_mut(&mid) else {
                    return;
                };
                let stream = &mut slot.stream;
                stream.last_packet = Instant::now();
                // The RTP clock is 48 kHz, as ours.
                stream.arrivals.arrived(data.time.numer() as u32, data.network_time);
                stream.heard |= data.data[..] != SILENCE[..];
                if !self.controls.deafened.load(Ordering::Relaxed) {
                    stream.jitter.insert(**data.seq_range.start() as u16, data.data.to_vec());
                }
            }
            _ => {}
        }
    }

    /// Media flows: the first time, the devices open.
    fn up(&mut self) {
        if self.connected {
            return;
        }
        if self.audio.is_none() {
            let (input, output) = self.controls.take_chosen();
            match Audio::open(input, output) {
                Ok(audio) => {
                    announce(&audio);
                    self.audio = Some(audio);
                    self.devices = Some(Devices::start(self.microphone_unasked));
                }
                Err(err) => {
                    let (reason, detail) = media::failure(err);
                    out::fail(reason, detail);
                }
            }
        } else if self.lost.is_some() {
            self.controls.cue(Cue::Back);
        }
        self.connected = true;
        self.lost = None;
        out::send(&MediaResponse::Connected { ssrc: 0, dave_version: 0 });
    }

    fn down(&mut self) {
        if !self.connected {
            return;
        }
        self.connected = false;
        self.lost = Some(Instant::now());
        self.controls.cue(Cue::Lost);
        out::send(&MediaResponse::Interrupted);
    }

    /// Sends one packet. Over TCP, the first one to an address opens the stream.
    fn transmit(&mut self, proto: Protocol, to: SocketAddr, packet: &[u8]) {
        match proto {
            Protocol::Udp => {
                if let Some(udp) = &self.link.udp {
                    // A send that fails is a lost packet; ICE notices a dead path.
                    if udp.send_to(packet, to).is_ok() {
                        self.meter.sent(packet.len());
                    }
                }
            }
            Protocol::Tcp => {
                if !self.link.tcp.contains_key(&to) {
                    let Ok(mut socket) = TcpStream::connect(to) else {
                        return;
                    };
                    let _ = socket.set_nodelay(true);
                    let token = Token(self.next_token);
                    self.next_token += 1;
                    if self.poll.registry().register(&mut socket, token, Interest::READABLE | Interest::WRITABLE).is_err() {
                        return;
                    }
                    self.link.tcp.insert(to, (token, Conn::new(socket)));
                }
                let failed = self.link.tcp.get_mut(&to).is_some_and(|(_, conn)| conn.send(packet).is_err());
                if failed {
                    self.drop_stream(to);
                } else {
                    self.meter.sent(packet.len());
                }
            }
            _ => {}
        }
    }

    fn drop_stream(&mut self, server: SocketAddr) {
        if let Some((_, mut conn)) = self.link.tcp.remove(&server) {
            let _ = self.poll.registry().deregister(&mut conn.socket);
        }
    }

    /// Reads what `token` has waiting, and flushes a stream that became writable.
    fn receive(&mut self, token: Token, writable: bool) {
        if token == UDP {
            let Some(udp) = &self.link.udp else {
                return;
            };
            let mut buf = [0; 2000];
            loop {
                match udp.recv_from(&mut buf) {
                    Ok((n, source)) => {
                        self.meter.received(n);
                        self.packets.push_back((Protocol::Udp, source, buf[..n].to_vec()));
                    }
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    // WouldBlock, or an ICMP error for something we sent: nothing more now.
                    Err(_) => break,
                }
            }
            return;
        }
        let Some(server) = self.link.tcp.iter().find(|(_, (held, _))| *held == token).map(|(server, _)| *server) else {
            return;
        };
        let Some((_, conn)) = self.link.tcp.get_mut(&server) else {
            return;
        };
        if writable && conn.flush().is_err() {
            self.drop_stream(server);
            return;
        }
        let (packets, meter) = (&mut self.packets, &mut self.meter);
        let read = conn.read(|frame| {
            meter.received(frame.len());
            packets.push_back((Protocol::Tcp, server, frame));
        });
        if !read {
            self.drop_stream(server);
        }
    }

    fn input(&mut self, (proto, source, packet): Packet) -> bool {
        if !self.link.rtc.is_alive() {
            return true;
        }
        let local = match proto {
            Protocol::Tcp => self.link.local_tcp,
            _ => self.link.local_udp,
        };
        let (Some(destination), Ok(contents)) = (local, packet.as_slice().try_into()) else {
            return true;
        };
        let receive = Receive { proto, source, destination, contents };
        self.link.rtc.handle_input(Input::Receive(Instant::now(), receive)).is_ok()
    }

    /// One Opus packet on the microphone's m-line, stamped `tick` × 960.
    fn write(&mut self, opus: &[u8], tick: u64, level: i8, voice: bool) {
        let Some(writer) = self.link.rtc.writer(self.link.microphone) else {
            return;
        };
        let Some(pt) = writer.payload_params().next().map(|params| params.pt()) else {
            return;
        };
        let time = MediaTime::new(tick * FRAME as u64, Frequency::FORTY_EIGHT_KHZ);
        let _ = writer.audio_level(level, voice).write(pt, Instant::now(), time, opus.to_vec());
    }

    fn encode(&mut self, frame: &[i16; FRAME_SAMPLES], tick: u64, voice: bool) {
        let mut packet = [0u8; 1275];
        let Some(len) = self.encoder.encode(frame, &mut packet) else {
            return;
        };
        // Two bytes or less is DTX saying there is nothing worth sending.
        if len > 2 {
            self.write(&packet[..len], tick, dbov(frame), voice);
        }
    }

    /// The microphone's 20 ms, through the processing and the gate. Muted, nothing goes out at
    /// all.
    fn capture(&mut self, audio: &mut Audio) -> u8 {
        let controls = self.controls.clone();
        self.encoder.set_bitrate(controls.bitrate());
        let muted = controls.muted.load(Ordering::Relaxed);
        let mut frame = [0i16; FRAME_SAMPLES];
        let (step, heard) = listen(&controls, audio, &mut self.processor, &mut self.gate, &mut frame);
        if let Some(speaking) = step.speaking {
            out::send(&MediaResponse::Speaking { user_id: self.user_id.clone(), speaking });
        }
        if muted || !self.connected {
            return heard;
        }
        let early = step.early.len() as u64;
        for (n, frame) in step.early.iter().enumerate() {
            self.encode(frame, self.ticks.saturating_sub(early - n as u64), true);
        }
        match step.send {
            Some(Send::Frame(frame)) => self.encode(&frame, self.ticks, true),
            Some(Send::Silence) => self.write(&SILENCE, self.ticks, -127, false),
            None => {}
        }
        heard
    }

    /// Everyone's 20 ms, mixed.
    fn play(&mut self, audio: &mut Audio) -> u8 {
        let deafened = self.controls.deafened.load(Ordering::Relaxed);
        let ear: Ear = self.controls.ear();
        let ducked = ear.ducking(self.slots.values().map(|slot| (slot.user.as_str(), slot.stream.speaking())));
        let mut mix = [0i32; FRAME_SAMPLES];
        for (mid, slot) in &mut self.slots {
            let loud = if deafened {
                slot.stream.drop_audio();
                slot.stream.heard
            } else {
                match slot.stream.pull(&format_args!("slot {mid}")) {
                    Some(frame) => {
                        slot.stream.mix_into(&mut mix, &frame, ear.factor(&slot.user, ducked));
                        level(&frame) >= VOICE
                    }
                    None => false,
                }
            };
            if let Some(speaking) = slot.stream.hear(loud) {
                if !slot.user.is_empty() {
                    out::send(&MediaResponse::Speaking { user_id: slot.user.clone(), speaking });
                }
            }
        }
        mixdown(&mix, &self.controls, &mut self.cue, audio, &mut self.processor, &mut self.limiter)
    }

    /// The 20 ms of a call whose devices are open. False when the call cannot go on.
    fn tick(&mut self) {
        self.ticks += 1;
        let Some(mut audio) = self.audio.take() else {
            return;
        };
        let input = self.capture(&mut audio);
        let output = self.play(&mut audio);
        if self.ticks.is_multiple_of(5) && self.controls.metering() && (input, output) != self.levels {
            self.levels = (input, output);
            out::send(&MediaResponse::Levels { input, output });
        }
        if self.ticks.is_multiple_of(SECOND) {
            audio.log_glitches();
            self.meter.troubles = audio.troubles();
        }
        if self.ticks.is_multiple_of(SECOND) && self.connected {
            let stats = self.meter.second(self.slots.values().map(|slot| slot.stream.counts()));
            self.controls.publish_stats(stats);
        }
        if let Some(devices) = &mut self.devices {
            if self.ticks.is_multiple_of(SECOND) {
                if let Err((reason, detail)) = devices.permission() {
                    out::fail(reason, detail);
                }
            }
            if let Err((reason, detail)) = devices.service(&mut audio, &self.controls, self.ticks) {
                out::fail(reason, detail);
            }
        }
        self.audio = Some(audio);
    }

    /// The bounds on waiting for media: to start after an answer, and to come back.
    fn overdue(&self) {
        if let Some(lost) = self.lost {
            if lost.elapsed() > COME_BACK {
                out::fail(CallFailure::Lost, DROPPED);
            }
        } else if !self.connected && self.audio.is_none() && self.link.answered.elapsed() > CONNECT {
            out::fail(CallFailure::Lost, NO_PATH);
        }
    }

    fn run(&mut self) {
        let mut events = Events::with_capacity(64);
        let mut next = Instant::now() + TICK;
        while !self.controls.hangup.load(Ordering::Relaxed) {
            let swapped = self.controls.tiny_link.lock().unwrap().take();
            if let Some(link) = swapped {
                self.take_up(link);
            }
            let map = self.controls.tiny_slots.lock().unwrap().take();
            if let Some(map) = map {
                self.apply_slots(&map);
            }
            // A connection str0m has given up on waits for the next one, or for the bound.
            let timeout = match self.drain() {
                Some(at) => at,
                None => {
                    self.down();
                    self.lost.get_or_insert_with(Instant::now);
                    next
                }
            };
            if let Some(packet) = self.packets.pop_front() {
                if !self.input(packet) {
                    eprintln!("mediad: str0m refused a packet");
                }
                continue;
            }
            let now = Instant::now();
            if now >= next {
                next += TICK;
                if now > next + 5 * TICK {
                    // The machine slept or stalled: do not replay the missed ticks.
                    next = now + TICK;
                }
                self.tick();
                if self.ticks.is_multiple_of(SECOND) {
                    self.overdue();
                }
                continue;
            }
            let wait = timeout.min(next).saturating_duration_since(now).max(Duration::from_millis(1));
            if let Err(err) = self.poll.poll(&mut events, Some(wait)) {
                if err.kind() != ErrorKind::Interrupted {
                    eprintln!("mediad: waiting on the call's sockets: {err}");
                    out::fail(CallFailure::Engine, ENGINE);
                }
            }
            for event in &events {
                self.receive(event.token(), event.is_writable());
            }
            if self.packets.is_empty() && self.link.rtc.is_alive() {
                let _ = self.link.rtc.handle_input(Input::Timeout(Instant::now()));
            }
        }
        if self.gate.talking() {
            out::send(&MediaResponse::Speaking { user_id: self.user_id.clone(), speaking: false });
        }
        if let Some(audio) = &mut self.audio {
            finish_cue(&self.controls, &mut self.cue, audio);
        }
        for (mid, slot) in &self.slots {
            let jitter = &slot.stream.jitter;
            eprintln!("mediad: slot {mid}: {} lost ({} rebuilt), {} late", jitter.lost, slot.stream.rebuilt, jitter.late);
        }
        if let Some(audio) = &self.audio {
            media::sign_off(audio);
        }
        if let Some(devices) = &mut self.devices {
            devices.stop();
        }
    }
}

/// Runs the call until chatd hangs up. A failure does not return (`out::fail` exits).
pub fn run(link: Link, user_id: String, controls: Arc<Controls>) {
    let mut microphone_unasked = false;
    if !Audio::simulated() {
        match microphone() {
            Some(Microphone::Denied) => out::fail(CallFailure::Audio, MICROPHONE_DENIED),
            Some(Microphone::Unasked) => microphone_unasked = true,
            Some(Microphone::Allowed) | None => {}
        }
    }
    // The tick is woken by the system timer: at 1 ms, not Windows' default 15.6 ms.
    let _timer = rt::FineTimer::hold();
    rt::tick_thread();
    let (Some(encoder), Ok(poll)) = (Encoder::voice(), Poll::new()) else {
        eprintln!("mediad: the Opus encoder or the poller could not be made");
        out::fail(CallFailure::Engine, ENGINE);
    };
    let mut call = Call {
        controls,
        user_id,
        link,
        poll,
        next_token: 1,
        packets: VecDeque::new(),
        connected: false,
        lost: None,
        audio: None,
        devices: None,
        microphone_unasked,
        encoder,
        gate: Gate::default(),
        slots: HashMap::new(),
        ticks: 0,
        levels: (0, 0),
        cue: None,
        processor: Processor::new(),
        limiter: Limiter::default(),
        meter: Meter::new(CallPath::Unknown),
    };
    if let Err(err) = call.register() {
        eprintln!("mediad: the call's socket: {err}");
        out::fail(CallFailure::Engine, ENGINE);
    }
    call.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(mid: &str, user: &str) -> TinySlot {
        TinySlot { mid: mid.into(), user_id: user.into() }
    }

    #[test]
    fn a_slot_that_changes_hands_starts_again_and_one_no_longer_held_goes() {
        let mut held = HashMap::new();
        assert_eq!(remap(&mut held, &[slot("a", "ada"), slot("b", "bea")]), (vec![], vec![]));
        assert_eq!(held.len(), 2);
        // The same map again changes nothing.
        assert_eq!(remap(&mut held, &[slot("b", "bea"), slot("a", "ada")]), (vec![], vec![]));
        // Bea left and Cy took her slot; Ada's is untouched.
        assert_eq!(remap(&mut held, &[slot("a", "ada"), slot("b", "cy")]), (vec!["b".to_owned()], vec![]));
        assert_eq!(held["b"], "cy");
        // Ada left and nobody took hers.
        assert_eq!(remap(&mut held, &[slot("b", "cy")]), (vec![], vec!["a".to_owned()]));
        assert!(!held.contains_key("a"));
        // Audio that came before the first map is named, not restarted.
        held.insert("c".into(), String::new());
        assert_eq!(remap(&mut held, &[slot("b", "cy"), slot("c", "dee")]), (vec![], vec![]));
        assert_eq!(held["c"], "dee");
    }

    #[test]
    fn a_huge_map_is_held_to_the_cap() {
        let map: Vec<TinySlot> = (0..500).map(|n| slot(&n.to_string(), "x")).collect();
        let mut held = HashMap::new();
        remap(&mut held, &map);
        assert_eq!(held.len(), SLOTS);
    }

    #[test]
    fn the_audio_level_is_in_dbov() {
        assert_eq!(dbov(&[0; 1920]), -127);
        assert_eq!(dbov(&[i16::MAX; 1920]), 0);
        let half: Vec<i16> = (0..1920).map(|n| if n % 2 == 0 { 16_384 } else { -16_384 }).collect();
        assert_eq!(dbov(&half), -6);
        assert_eq!(dbov(&[3; 1920]), -81);
    }

    fn spec(slots: u16) -> TinyOffer {
        TinyOffer { user_id: "me".into(), host: "127.0.0.1".into(), slots, muted: false, deafened: false }
    }

    /// One microphone and `slots` receive slots, Opus only, and both kinds of candidate.
    #[test]
    fn the_offer_is_one_microphone_then_a_receive_slot_for_each_other_seat() {
        let offer = Offer::new(&spec(11)).unwrap();
        let sdp = offer.sdp();
        assert_eq!(sdp.matches("m=audio").count(), 12, "{sdp}");
        assert_eq!(sdp.matches("a=sendonly").count(), 1);
        assert_eq!(sdp.matches("a=recvonly").count(), 11);
        assert!(!sdp.contains("m=video") && !sdp.contains("m=application"));
        let codecs: Vec<&str> = sdp.lines().filter(|line| line.starts_with("a=rtpmap:")).collect();
        assert!(codecs.iter().all(|line| line.contains("opus/48000/2")), "{codecs:?}");
        assert!(sdp.contains("ssrc-audio-level"), "the RFC 6464 extension is offered");
        assert!(sdp.contains(" udp ") || sdp.contains(" UDP "), "a UDP host candidate");
        assert!(sdp.contains("tcptype active"), "and an active ICE-TCP one");
        assert!(sdp.contains("127.0.0.1"), "on the address that reaches the server");
        assert!(sdp.len() < 32 * 1024, "under the server's cap");
        assert!(!sdp.contains("ice-lite"), "we are the full ICE agent");
    }

    #[test]
    fn an_answer_that_is_not_sdp_is_refused() {
        let offer = Offer::new(&spec(1)).unwrap();
        assert!(offer.accept("not sdp").is_err());
    }
}
