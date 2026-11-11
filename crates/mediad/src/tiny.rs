//! A call on a Tiny server: WebRTC to the server's SFU through str0m, a sans-IO library this
//! process drives from its own loop. tinyserver's `docs/calls.md` is the contract, and its
//! headless client (`crates/sfu/src/client.rs`) is what this follows.
//!
//! 1. `TinyOffer` makes the offer ([`Offer::new`]): one UDP socket, a host candidate for it and
//!    an active ICE-TCP candidate (RFC 6544) on the same address, then one `sendonly` audio
//!    m-line for the microphone and one `recvonly` per receive slot. Audio is Opus. Video is
//!    H.264 constrained baseline: a simulcast camera and one share, both inactive until asked,
//!    and one `recvonly` picture. We are the full
//!    ICE agent and the server is ICE-lite; no trickle and no STUN server, since the server
//!    learns our address from our checks. A UDP pair outranks a TCP one, so TCP carries the call
//!    only where UDP cannot get through, as on a network that blocks it.
//! 2. chatd sends the offer as `call.join`, and hands back the answer ([`Offer::accept`]) and
//!    every `call.slots`.
//! 3. [`run`] is the call: a 20 ms tick as in `media`, with str0m's timers and packets in
//!    between. The microphone goes through the same speech gate, as mono Opus with each frame's
//!    RFC 6464 level, stamped n × 960 at 48 kHz. Muting sends the faded frame and the silence
//!    trail, then nothing. Each frame that comes in goes to its slot's jitter buffer by its
//!    RTP time, so a gap the SFU's new sequence numbers hide is still a missing frame. A slot
//!    that changes hands starts both afresh, since the new speaker's RTP times are not the last one's.
//!
//! The devices open only once media flows, so a join that fails never lights the microphone
//! indicator. The microphone goes through the same processing as a Discord call (`process`):
//! the mix played is the echo canceller's reference. Each voice is played at the volume the
//! user chose for that person, ducked under a priority speaker (`hear`); the encoder runs at the
//! room's bitrate; and str0m's statistics, once a second, give the round trip and the path for
//! `Stats`.
//!
//! A path that dies is noticed from what comes back, not from voice: see [`SILENT`].

use crate::audio::{level, Audio};
use crate::cue::Playing;
use crate::framing::Conn;
use crate::hear::{Ear, Limiter};
use crate::media::{self, announce, finish_cue, listen, mixdown, Devices, Gate, Send, Stream, MICROPHONE_DENIED};
use crate::opus::Encoder;
use crate::permission::{microphone, Microphone};
use crate::process::Processor;
use crate::rtp::SILENCE;
use crate::session::Controls;
use crate::stats::Meter;
use crate::video::{self, Rates, Received, Track, Video, Wants, Word};
use crate::{out, rt, FRAME, FRAME_SAMPLES};
use mio::net::{TcpStream, UdpSocket};
use mio::{Events, Interest, Poll, Token};
use protocol::{CallFailure, CallPath, Cue, MediaResponse, TinyOffer, TinyPicture, TinySlot};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaData, MediaKind, MediaTime, Mid, Rid, Simulcast, SimulcastLayer};
use str0m::net::{Protocol, Receive, TcpType};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

const TICK: Duration = Duration::from_millis(20);
const SECOND: u64 = 50;

/// H.264 constrained baseline, level 5.1, packetization mode 1. The SFU accepts this profile
/// and nothing else on a video m-line. Level 5.1 holds 2560×1440 at 60 fps, the most a share
/// sends; the level is a negotiable parameter (RFC 6184 §8.2.2), so a server that still says
/// level 3.1 matches all the same.
const H264_PROFILE: u32 = 0x42e033;
const H264_PT: u8 = 108;
const H264_RTX_PT: u8 = 109;

/// The bandwidth estimate a connection starts from, until the server's feedback says better.
const START_BPS: u64 = 1_000_000;
/// The most between STUN checks, and so how often a working path is checked, even in silence.
/// str0m's 3 s, with its 9 tries, takes up to 21 s to call a path dead.
const STUN_MAX_RTO: Duration = Duration::from_secs(1);
/// Unanswered checks before a path is dead. About 7 s on a path that was working and 6 s on one
/// that never was: [`SILENT`] speaks first on the one, and this bounds a new link on the other,
/// with room for a slow first connect (TCP included) and for a lossy network's run of bad luck.
const STUN_RETRANSMITS: usize = 7;
/// A link that was up and has answered no STUN check for this long is lost, though ICE has not
/// said so yet. Voice cannot say: a quiet room sends none, and the gate sends nothing between
/// words. Nor can the server's RTCP, which goes on arriving on a path that only died our way.
/// STUN can: the server, ICE-lite, sends it only to answer us, each [`STUN_MAX_RTO`] in any room,
/// and by now several checks, retries included, have gone unanswered.
const SILENT: Duration = Duration::from_secs(3);

/// Opus for audio, H.264 for video, and no hold for a missing audio packet. str0m's default
/// waits for 15 later packets or a second, and the talk spurt has already ended by then.
/// Video keeps its own hold. Bandwidth estimation from the server's TWCC feedback is what turns
/// on str0m's pacer: a keyframe leaves spread out, and audio, which it does not pace, goes
/// around it. It looks for more bandwidth only once there is video to spend it on.
/// Where the time of a join goes inside mediad: one line per stage, once per offer, as
/// milliseconds since the offer came in. chatd tells its own stages from the click
/// (`call join` lines in `chatd.log`); these are the ones after the offer.
pub mod stages {
    use std::sync::Mutex;
    use std::time::Instant;

    static OFFER: Mutex<Option<(Instant, Vec<&'static str>)>> = Mutex::new(None);

    /// A new offer: the clock starts and every stage may be told again.
    pub fn begin() {
        *OFFER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((Instant::now(), Vec::new()));
    }

    /// `name` happened; told the first time only.
    pub fn mark(name: &'static str) {
        let mut offer = OFFER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((began, told)) = offer.as_mut() {
            if !told.contains(&name) {
                told.push(name);
                tracing::info!(stage = name, since_offer_ms = began.elapsed().as_millis() as u64, "call join");
            }
        }
    }
}

/// The DTLS certificate every connection of this process uses. Making one is key generation,
/// which was done inside each offer; it is made once, by [`warm_up`] while the process waits for
/// its call. One certificate for the process is what a browser does for its tabs, and a
/// connection is still keyed afresh by its own handshake.
fn dtls_cert() -> Option<str0m::crypto::dtls::DtlsCert> {
    static CERT: std::sync::OnceLock<Option<str0m::crypto::dtls::DtlsCert>> = std::sync::OnceLock::new();
    CERT.get_or_init(|| str0m::crypto::from_feature_flags().dtls_provider.generate_certificate()).clone()
}

/// What a call needs ready, done on a thread of its own as soon as the process starts.
pub fn warm_up() {
    let started = Instant::now();
    let _ = dtls_cert();
    tracing::debug!(elapsed_ms = started.elapsed().as_millis() as u64, "mediad: certificate ready");
}

fn rtc_config() -> RtcConfig {
    let mut config = RtcConfig::new()
        .clear_codecs()
        .enable_opus(true, false)
        .set_stats_interval(Some(Duration::from_secs(1)))
        .enable_bwe(Some(Bitrate::bps(START_BPS)))
        .set_reordering_size_audio(0)
        .set_reordering_timeout_audio(Some(Duration::from_millis(40)));
    config.codec_config().add_h264(H264_PT.into(), Some(H264_RTX_PT.into()), true, H264_PROFILE);
    config.set_max_stun_rto(STUN_MAX_RTO);
    config.set_max_stun_retransmits(STUN_RETRANSMITS);
    match dtls_cert() {
        Some(cert) => config.set_dtls_cert(cert),
        None => config,
    }
}

/// Padding on an RTX stream. str0m keeps 50 kb/s or more of it going on any line that has RTX,
/// inactive or not, once any media flows, with no way to turn it off, so a call sending no video
/// has to throw it away here (see [`Call::transmit`]). Only RTP is ever this byte: RTCP's types
/// start at 192 and STUN and DTLS leave the top two bits clear.
fn is_padding(packet: &[u8]) -> bool {
    matches!(packet, [first, second, ..] if first >> 6 == 2 && second & 0x7f == H264_RTX_PT)
}

/// Video RTP handed to the transport, and whether it ends an original frame. Padding is
/// not video; retransmissions spend bandwidth but must not inflate the frame rate.
fn video_packet(packet: &[u8]) -> Option<bool> {
    let [first, second, ..] = packet else { return None };
    if packet.len() < 12 || first >> 6 != 2 || first & 0x20 != 0 {
        return None;
    }
    match second & 0x7f {
        H264_PT => Some(second & 0x80 != 0),
        H264_RTX_PT => Some(false),
        _ => None,
    }
}

/// What str0m's `egress_loss_fraction` looks back over, by when packets were sent.
const EGRESS_LOSS_SPAN: Duration = Duration::from_secs(1);

/// What TWCC says the server missed of what we sent, when that can be believed. str0m numbers
/// padding for feedback before [`Call::transmit`] throws it away, so anything sent within the
/// span of a dropped packet reads as lost, on a clean path about half of it, and for a second
/// after video starts as well. The microphone's receiver reports stand in until then.
fn egress_loss(fraction: Option<f32>, dropped: Option<Instant>, now: Instant) -> Option<f32> {
    fraction.filter(|_| dropped.is_none_or(|at| now.saturating_duration_since(at) > EGRESS_LOSS_SPAN))
}

/// The jitter buffer's sequence for an RTP timestamp at 48 kHz. One 20 ms frame is 960.
fn frame_sequence(timestamp: u64) -> u16 {
    (timestamp / FRAME as u64) as u16
}

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

const NO_PATH: &str = "The server answered, but call audio couldn't get through. A VPN or firewall may be blocking it.";
const ENGINE: &str = "The call engine could not start. Try again.";

/// Which candidates to offer. `TINYCLIENT_MEDIAD_ICE` makes a test offer one kind alone: `udp`,
/// or `tcp`, which is how a network that blocks UDP looks to the server.
fn transports() -> (bool, bool) {
    match crate::out::var("TINYCLIENT_MEDIAD_ICE").as_deref() {
        Some("udp") => (true, false),
        Some("tcp") => (false, true),
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
    /// Camera, off until a renegotiation turns it sendonly. Simulcast rids `h` and `l`.
    camera: Mid,
    /// The one share, off until a renegotiation turns it sendonly. No rids.
    share: Mid,
    /// The one picture the room forwards: a share, or else the active speaker's camera.
    screen: Mid,
    /// A renegotiation offer that has not been answered. The join's offer lives on [`Offer`].
    pending: Option<SdpPendingOffer>,
    /// The m-line and direction the pending offer changed from, so a refusal can undo it.
    /// str0m flips the m-line when it builds the offer.
    flipped: Option<(Mid, Direction)>,
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
        stages::begin();
        let ip = local_ip(&spec.host);
        let (udp, tcp) = transports();
        let mut rtc = rtc_config().build(Instant::now());
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
        // str0m checks a new mid against the session's, not the others in this change. So many
        // random mids of three characters collide about once in 2,000 offers, and two m-lines on
        // one mid break the next offer. An unapplied change leaves `rtc` as it was.
        let (change, microphone, camera, share, screen) = loop {
            let mut change = rtc.sdp_api();
            let mut mids = HashSet::new();
            let mut add = |direction, kind, layers| {
                let mid = change.add_media(kind, direction, None, None, layers);
                mids.insert(mid);
                mid
            };
            let microphone = add(Direction::SendOnly, MediaKind::Audio, None);
            for _ in 0..spec.slots {
                add(Direction::RecvOnly, MediaKind::Audio, None);
            }
            let mut layers = Simulcast::new();
            layers.add_send_layer(SimulcastLayer::new("h"));
            layers.add_send_layer(SimulcastLayer::new("l"));
            let camera = add(Direction::Inactive, MediaKind::Video, Some(layers));
            let share = add(Direction::Inactive, MediaKind::Video, None);
            let screen = add(Direction::RecvOnly, MediaKind::Video, None);
            if mids.len() == spec.slots as usize + 4 {
                break (change, microphone, camera, share, screen);
            }
        };
        let (offer, pending) = change.apply().ok_or_else(|| io::Error::other("nothing to offer"))?;
        let link = Link {
            rtc,
            udp: socket,
            tcp: HashMap::new(),
            local_udp,
            local_tcp,
            microphone,
            camera,
            share,
            screen,
            pending: None,
            flipped: None,
            answered: Instant::now(),
        };
        stages::mark("offer_built");
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
        stages::mark("answer_applied");
        Ok(self.link)
    }
}

impl Link {
    pub fn microphone_mid(&self) -> Mid {
        self.microphone
    }

    pub fn screen_mid(&self) -> Mid {
        self.screen
    }

    pub fn camera_mid(&self) -> Mid {
        self.camera
    }

    pub fn share_mid(&self) -> Mid {
        self.share
    }

    /// A camera, share, or picture m-line. Those are not voice slots.
    pub fn is_picture(&self, mid: Mid) -> bool {
        mid == self.camera || mid == self.share || mid == self.screen
    }

    /// A renegotiation offer is out and has not been answered.
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// `share` selects the share m-line; otherwise the camera.
    pub fn direction(&self, share: bool) -> Direction {
        let mid = if share { self.share } else { self.camera };
        self.rtc.media(mid).map(|media| media.direction()).unwrap_or(Direction::Inactive)
    }

    /// One renegotiation: the camera or the share becomes `sendonly` or goes back to `inactive`.
    /// The SDP is the offer for `call.offer`. Frames wait until [`Link::take_answer`].
    pub fn offer_send(&mut self, share: bool, on: bool) -> Result<String, String> {
        if self.pending.is_some() {
            return Err("an offer is already out".to_owned());
        }
        let mid = if share { self.share } else { self.camera };
        let direction = if on { Direction::SendOnly } else { Direction::Inactive };
        self.flipped = self.rtc.media(mid).map(|media| (mid, media.direction()));
        let mut change = self.rtc.sdp_api();
        change.set_direction(mid, direction);
        match change.apply() {
            Some((offer, pending)) => {
                self.pending = Some(pending);
                Ok(offer.to_sdp_string())
            }
            None => {
                self.flipped = None;
                Err("nothing changed".to_owned())
            }
        }
    }

    /// The answer to the last [`Link::offer_send`]. An empty SDP is a refusal (`share_taken`):
    /// the m-line goes back to the direction it had, and the offer is dropped.
    pub fn take_answer(&mut self, sdp: &str) -> Result<(), String> {
        let Some(pending) = self.pending.take() else {
            return Err("no offer is out".to_owned());
        };
        if sdp.is_empty() {
            self.undo();
            return Err("refused".to_owned());
        }
        let answer = SdpAnswer::from_sdp_string(sdp).map_err(|err| format!("{err:?}"))?;
        match self.rtc.sdp_api().accept_answer(pending, answer) {
            Ok(()) => {
                self.flipped = None;
                Ok(())
            }
            Err(err) => {
                self.undo();
                Err(format!("{err:?}"))
            }
        }
    }

    /// Puts the m-line back. str0m already flipped it when the offer was made, and the server
    /// kept the state from before that offer.
    fn undo(&mut self) {
        self.pending = None;
        if let Some((mid, direction)) = self.flipped.take() {
            let mut change = self.rtc.sdp_api();
            change.set_direction(mid, direction);
            let _ = change.apply();
        }
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

/// What the call's loop does next.
#[derive(Debug, PartialEq)]
enum Next {
    Tick,
    Packet,
    Wait,
}

/// A tick that is due goes first, whatever has queued up: a flood of packets may delay a packet,
/// and may not delay the microphone or the speakers.
fn next_step(now: Instant, tick_at: Instant, packets: bool) -> Next {
    if now >= tick_at {
        Next::Tick
    } else if packets {
        Next::Packet
    } else {
        Next::Wait
    }
}

/// When the tick after the one due at `tick_at` runs. The machine slept or stalled: the missed
/// ticks are not replayed.
fn after(tick_at: Instant, now: Instant) -> Instant {
    let next = tick_at + TICK;
    if now > next + 5 * TICK {
        now + TICK
    } else {
        next
    }
}

/// Voice frames held at most while the encryption is busy, each way: half a second of one
/// voice. Past it the oldest is dropped, as a frame with no keys is.
const HELD: usize = 25;
/// The same half second of a full room's voices, on their way in.
const HELD_IN: usize = HELD * 12;

/// A frame of ours waiting its turn at the encryption.
struct Unsealed {
    opus: Vec<u8>,
    tick: u64,
    level: i8,
    voice: bool,
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
    /// Voice the encryption could not take yet, in order. An MLS commit holds it for a moment,
    /// and the tick does not wait for that: these go first thing on the next turn of the loop.
    unsealed: VecDeque<Unsealed>,
    unopened: VecDeque<MediaData>,
    /// Media flows on this link.
    connected: bool,
    /// This drop was already told to chatd. Cleared when media flows, or when a new link takes
    /// over, so that link can tell of its own death.
    announced: bool,
    /// Lost media, and since when. chatd resumes the call; nothing here ends it on a clock.
    lost: Option<Instant>,
    /// When the link last answered a STUN check.
    heard: Instant,
    /// [`SILENT`] called this link lost. ICE never did, so ICE will not say if it came back.
    silenced: bool,
    audio: Option<Audio>,
    /// The devices being opened on a thread of their own since the answer came in, so that
    /// they are ready when media is ([`open_beside_connecting`]).
    opening: Option<std::sync::mpsc::Receiver<Result<Audio, crate::audio::AudioError>>>,
    devices: Option<Devices>,
    microphone_unasked: bool,
    encoder: Encoder,
    /// Whether the encoder was made with Opus Audio (High quality) vs Voip.
    encoder_audio_app: bool,
    gate: Gate,
    slots: HashMap<String, Slot>,
    ticks: u64,
    levels: (u8, u8),
    cue: Option<Playing>,
    processor: Processor,
    limiter: Limiter,
    meter: Meter,
    /// The picture's threads. This loop only moves their bytes.
    video: Video,
    /// What the path can take, from TWCC feedback. `None` until this connection has said.
    estimate: Option<u64>,
    /// What the tracks may spend now, and what the estimator was told to look for.
    rates: Rates,
    /// Whether that estimate is to be believed.
    uplink: video::Uplink,
    /// Video bytes handed to str0m so far.
    video_bytes: u64,
    desired: u64,
    /// When padding was last thrown away ([`Call::transmit`]).
    padding_dropped: Option<Instant>,
    /// What the last accepted answer left sending. str0m's direction flips before the answer.
    sending_share: bool,
    sending_camera: bool,
    /// `Some(true)` is a share offer still out; `Some(false)` is a camera offer.
    offered: Option<bool>,
    pli_at: Option<Instant>,
    shown: Option<TinyPicture>,
    watching: bool,
    share_noted: bool,
    camera_noted: bool,
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
            tracing::warn!("mediad: the new connection's socket: {err}");
        }
        self.packets.clear();
        self.unsealed.clear();
        self.unopened.clear();
        // Still connected as far as we could tell (the server stopped hearing us, or restarted
        // before ICE noticed): this is the loss, and it gets its cue as `down` gives one. The
        // Back cue follows when media flows on the new link (`up`).
        if self.connected {
            self.connected = false;
            self.controls.cue(Cue::Lost);
            out::send(&MediaResponse::Interrupted);
        }
        self.lost.get_or_insert_with(Instant::now);
        self.silenced = false;
        self.sending_share = false;
        self.sending_camera = false;
        self.offered = None;
        self.estimate = None;
        self.rates = Rates::default();
        self.uplink = video::Uplink::default();
        self.video_bytes = 0;
        self.desired = 0;
        self.padding_dropped = None;
        self.video.reset();
        self.shown = None;
        // The new link has not been announced dead. A death of the one we just left already was.
        self.announced = false;
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
                    if let Some(stream) = Stream::clocked() {
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
            Event::Connected => {
                tracing::info!(state = "dtls_connected", "media state");
                stages::mark("dtls_connected");
                self.up();
            }
            Event::IceConnectionStateChange(IceConnectionState::Checking) => {
                tracing::info!(state = "ice_checking", "media state");
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => self.fell(),
            Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed) => {
                tracing::info!(state = "ice_connected", "media state");
                stages::mark("ice_connected");
                if self.lost.is_some() && self.audio.is_some() {
                    self.up();
                }
            }
            Event::PeerStats(stats) => {
                let pair = stats.selected_candidate_pair.as_ref();
                self.meter.round_trip(pair.and_then(|pair| pair.current_round_trip_time), stats.rtt);
                let egress = egress_loss(stats.egress_loss_fraction, self.padding_dropped, Instant::now());
                self.meter.path_loss(egress, stats.ingress_loss_fraction);
                let sent = video::Sent { media: stats.bytes_tx, video: self.video_bytes, estimate: self.estimate, loss: egress, rtt: stats.rtt };
                self.uplink.heard(sent, Instant::now());
                // Only host candidates are offered: the path is UDP or ICE-TCP, never a relay.
                if let Some(pair) = pair {
                    self.meter.path = match pair.protocol {
                        Protocol::Udp => CallPath::Udp,
                        _ => CallPath::Tcp,
                    };
                }
            }
            // What the server sees of our microphone: its FEC is planned for that loss, and it
            // is the path's own word on loss for a call that drops its padding (`egress_loss`).
            Event::MediaEgressStats(stats) if stats.mid == self.link.microphone => {
                if self.meter.spoke(stats.packets) {
                    if let Some(loss) = stats.loss {
                        self.encoder.set_loss(loss);
                        self.meter.path_loss(Some(loss), None);
                    }
                    if let Some(remote) = stats.remote {
                        self.meter.uplink_jitter(remote.jitter);
                    }
                }
            }
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => self.estimate = Some(estimate.as_u64()),
            Event::MediaData(data) if self.link.is_picture(data.mid) => self.picture_in(data),
            Event::KeyframeRequest(request) if request.mid == self.link.share_mid() => {
                tracing::debug!(kind = ?request.kind, sending = self.sending_share, "keyframe asked for the share");
                self.video.ask_key(Track::Share);
            }
            Event::KeyframeRequest(request) if request.mid == self.link.camera_mid() => match request.rid {
                Some(rid) if rid == Rid::from("l") => self.video.ask_key(Track::Low),
                Some(_) => self.video.ask_key(Track::High),
                None => {
                    self.video.ask_key(Track::High);
                    self.video.ask_key(Track::Low);
                }
            },
            Event::MediaData(data) => {
                stages::mark("first_media_frame");
                if self.unopened.len() >= HELD_IN {
                    self.unopened.pop_front();
                }
                self.unopened.push_back(data);
                self.open_voice();
            }
            _ => {}
        }
    }

    /// Voice in, as far as the encryption will take it now.
    fn open_voice(&mut self) {
        while let Some(data) = self.unopened.front() {
            let mid = data.mid.to_string();
            if !self.slots.contains_key(&mid) {
                // Audio before the map that names its speaker: heard, and named later.
                if self.slots.len() >= SLOTS {
                    self.unopened.pop_front();
                    continue;
                }
                let Some(stream) = Stream::clocked() else {
                    self.unopened.pop_front();
                    continue;
                };
                self.slots.insert(mid.clone(), Slot { user: String::new(), stream });
            }
            let Some(slot) = self.slots.get_mut(&mid) else {
                self.unopened.pop_front();
                continue;
            };
            // End-to-end encryption's half of the frame transform: what does not open with
            // its sender's key is dropped here, before the jitter buffer.
            let Some(opened) = self.controls.try_e2ee(|e2ee| e2ee.open(&slot.user, &data.data)) else {
                return;
            };
            let Some(data) = self.unopened.pop_front() else {
                return;
            };
            let Some(opus) = opened else {
                continue;
            };
            let stream = &mut slot.stream;
            stream.last_packet = Instant::now();
            // The RTP clock is 48 kHz, as ours.
            stream.arrivals.arrived(data.time.numer() as u32, data.network_time);
            stream.heard |= opus[..] != SILENCE[..];
            if opus[..] == SILENCE[..] {
                stream.arrivals.paused();
            }
            if !self.controls.deafened.load(Ordering::Relaxed) {
                // The SFU writes a fresh sequence, so a lost frame is a hole in the RTP
                // time and not in `seq_range`. The jitter buffer only sees sequence numbers.
                stream.jitter.insert(frame_sequence(data.time.numer()), opus);
            }
        }
    }

    /// Media flows: the first time, the devices open.
    fn up(&mut self) {
        if self.connected {
            return;
        }
        if self.audio.is_none() {
            // Opened beside ICE and DTLS when that worked, else now.
            let opened = match self.opening.take().and_then(|opening| opening.recv().ok()) {
                Some(opened) => opened,
                None => {
                    let (input, output) = self.controls.take_chosen();
                    Audio::open(input, output)
                }
            };
            match opened {
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
        self.announced = false;
        self.lost = None;
        self.silenced = false;
        // Opening the devices above can take longer than the watchdog waits.
        self.heard = Instant::now();
        out::send(&MediaResponse::Connected { ssrc: 0, dave_version: 0 });
    }

    /// No answer on a link that was up: lost, without waiting for ICE to run out of tries. It
    /// goes the way any drop does, so the dock says Reconnecting and chatd joins again. One that
    /// answers again before the new link is here carries on.
    fn watch_link(&mut self, now: Instant) {
        let silent = now.saturating_duration_since(self.heard) >= SILENT;
        if self.connected && silent {
            tracing::warn!("mediad: the server answered no STUN check for {SILENT:?}");
            self.silenced = true;
            self.fell();
        } else if self.silenced && !silent {
            tracing::warn!("mediad: the server is heard again");
            self.up();
        }
    }

    /// The link died, or never came up after we had already been in the call. Told once per
    /// drop: chatd stays in the call and brings it back. A join that has not connected yet is
    /// still the first-connect bound's to end.
    fn fell(&mut self) {
        if self.announced {
            return;
        }
        let was_up = self.connected;
        if !was_up && self.audio.is_none() {
            return;
        }
        self.announced = true;
        if was_up {
            self.connected = false;
            self.controls.cue(Cue::Lost);
        }
        self.lost.get_or_insert_with(Instant::now);
        out::send(&MediaResponse::Interrupted);
    }

    /// Sends one packet. Over TCP, the first one to an address opens the stream. With no video
    /// to send, a voice call does not pay for padding. The feedback then counts it as lost: the
    /// estimate, which nothing spends until video does, starts afresh then
    /// ([`Call::picture_step`]), and the meter leaves it out ([`egress_loss`]).
    fn transmit(&mut self, proto: Protocol, to: SocketAddr, packet: &[u8]) {
        if self.desired == 0 && is_padding(packet) {
            self.padding_dropped = Some(Instant::now());
            return;
        }
        match proto {
            Protocol::Udp => {
                if let Some(udp) = &self.link.udp {
                    // A send that fails is a lost packet; ICE notices a dead path.
                    if udp.send_to(packet, to).is_ok() {
                        self.sent_packet(packet);
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
                match self.link.tcp.get_mut(&to).map(|(_, conn)| conn.send(packet)) {
                    Some(Ok(())) => self.sent_packet(packet),
                    Some(Err(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {},
                    Some(Err(_)) => self.drop_stream(to),
                    None => {},
                }
            }
            _ => {}
        }
    }

    fn sent_packet(&mut self, packet: &[u8]) {
        self.meter.sent(packet.len());
        if let Some(ends_frame) = video_packet(packet) {
            self.controls.video_stats.lock().unwrap().sent(packet.len(), ends_frame, Instant::now());
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
        // RFC 7983: a first byte below 4 is STUN.
        if packet.first().is_some_and(|byte| *byte < 4) {
            self.heard = Instant::now();
        }
        let receive = Receive { proto, source, destination, contents };
        self.link.rtc.handle_input(Input::Receive(Instant::now(), receive)).is_ok()
    }

    /// One Opus packet on the microphone's m-line, stamped `tick` × 960.
    fn write(&mut self, opus: &[u8], tick: u64, level: i8, voice: bool) {
        if self.unsealed.len() >= HELD {
            self.unsealed.pop_front();
        }
        self.unsealed.push_back(Unsealed { opus: opus.to_vec(), tick, level, voice });
        self.seal_voice();
    }

    /// Voice out, as far as the encryption will take it now.
    fn seal_voice(&mut self) {
        while let Some(frame) = self.unsealed.front() {
            // The other half: sealed for the room's group, or held back while it has no keys.
            let Some(sealed) = self.controls.try_e2ee(|e2ee| e2ee.seal(&frame.opus)) else {
                return;
            };
            let Some(frame) = self.unsealed.pop_front() else {
                return;
            };
            let Some(payload) = sealed else {
                continue;
            };
            let Some(writer) = self.link.rtc.writer(self.link.microphone) else {
                continue;
            };
            let Some(pt) = writer.payload_params().next().map(|params| params.pt()) else {
                continue;
            };
            let time = MediaTime::new(frame.tick * FRAME as u64, Frequency::FORTY_EIGHT_KHZ);
            let _ = writer.audio_level(frame.level, frame.voice).write(pt, Instant::now(), time, payload);
            stages::mark("first_sealed_frame");
        }
    }

    /// One renegotiation at a time, then frames on the lines whose answers are in.
    fn picture_step(&mut self) {
        if let Some(sdp) = self.controls.take_picture_answer() {
            let refused = sdp.is_empty();
            let offered_share = self.offered == Some(true);
            let _ = self.link.take_answer(&sdp);
            self.sending_share = self.link.direction(true) == Direction::SendOnly;
            self.sending_camera = self.link.direction(false) == Direction::SendOnly;
            self.offered = None;
            if refused {
                if offered_share {
                    self.controls.set_share(self.sending_share);
                    out::send(&MediaResponse::Notice { detail: "Someone else is already sharing.".to_owned() });
                } else {
                    self.controls.set_camera(self.sending_camera);
                }
            }
            out::send(&MediaResponse::Sending { share: self.sending_share, camera: self.sending_camera });
        }
        if let Some(slot) = self.controls.take_picture() {
            let changed = slot.as_ref().map(|pictured| (pictured.user_id.clone(), pictured.kind)) != self.shown.as_ref().map(|pictured| (pictured.user_id.clone(), pictured.kind));
            self.shown = slot;
            if changed {
                self.video.reset();
                self.ask_keyframe();
            }
        }
        let watching = self.controls.watch();
        if self.watching && !watching {
            self.video.clear();
        }
        if watching {
            self.video.watch();
        }
        self.watching = watching;
        self.controls.video_stats.lock().unwrap().expect(watching && self.shown.is_some(), Instant::now());
        if self.offered.is_none() && !self.link.pending() {
            let (want_share, want_camera) = (self.controls.share(), self.controls.camera());
            if want_share != self.sending_share {
                if want_share {
                    crate::picture::retry_codec();
                } else {
                    self.share_noted = false;
                }
                match self.link.offer_send(true, want_share) {
                    Ok(sdp) => {
                        self.offered = Some(true);
                        out::send(&MediaResponse::TinyRenegotiate { sdp });
                    }
                    Err(err) => tracing::warn!("mediad: the share could not be offered: {err}"),
                }
            } else if want_camera != self.sending_camera {
                if want_camera {
                    crate::picture::retry_codec();
                } else {
                    self.camera_noted = false;
                }
                match self.link.offer_send(false, want_camera) {
                    Ok(sdp) => {
                        self.offered = Some(false);
                        out::send(&MediaResponse::TinyRenegotiate { sdp });
                    }
                    Err(err) => tracing::warn!("mediad: the camera could not be offered: {err}"),
                }
            }
        }
        let keep_camera = self.controls.camera();
        let share = self.connected && self.sending_share && self.controls.share();
        let camera = self.connected && self.sending_camera && keep_camera;
        let desired = video::desired(share, camera);
        if let Some(estimate) = self.estimate {
            let estimate = self.uplink.usable(estimate, desired, Instant::now());
            self.rates = video::allocate(estimate, share, camera, self.rates);
        }
        if desired != self.desired {
            if self.desired == 0 {
                self.link.rtc.bwe().reset(Bitrate::bps(START_BPS));
                self.estimate = None;
                self.rates = Rates::default();
            }
            self.desired = desired;
            self.link.rtc.bwe().set_desired_bitrate(Bitrate::bps(desired));
        }
        self.video.want(Wants { share, camera, keep_camera, rates: self.rates });
        while let Some(word) = self.video.next() {
            match word {
                Word::Frame { track, payload, rtp, at } => self.write_video(track, payload, rtp, at),
                Word::Failed { share: true, words } => {
                    if !self.share_noted {
                        self.share_noted = true;
                        self.controls.set_share(false);
                        out::send(&MediaResponse::Notice { detail: words });
                        out::send(&MediaResponse::Sending { share: false, camera: self.sending_camera });
                    }
                }
                Word::SelfView { share, painted } => out::send(&match painted {
                    Some(painted) => MediaResponse::SelfView { path: painted.path.display().to_string(), width: painted.width, height: painted.height, generation: painted.generation, share },
                    None => MediaResponse::SelfView { path: String::new(), width: 0, height: 0, generation: 0, share },
                }),
                Word::Failed { share: false, words } => {
                    if !self.camera_noted {
                        self.camera_noted = true;
                        self.controls.set_camera(false);
                        out::send(&MediaResponse::Notice { detail: words });
                        out::send(&MediaResponse::Sending { share: self.sending_share && self.controls.share(), camera: false });
                    }
                }
            }
        }
        if self.video.key_wanted() && watching {
            self.ask_keyframe();
        }
    }

    /// One sealed frame on its way. One that outlived what it was for (a reconnect, a line
    /// turned off) goes no further.
    fn write_video(&mut self, track: Track, payload: Vec<u8>, rtp: u64, at: Instant) {
        let (mid, rid, sending) = match track {
            Track::Share => (self.link.share_mid(), None, self.sending_share),
            Track::High => (self.link.camera_mid(), Some("h"), self.sending_camera),
            Track::Low => (self.link.camera_mid(), Some("l"), self.sending_camera),
        };
        if !sending {
            return;
        }
        let Some(writer) = self.link.rtc.writer(mid) else {
            return;
        };
        let Some(pt) = writer.payload_params().next().map(|params| params.pt()) else {
            return;
        };
        let writer = match rid {
            Some(rid) => writer.rid(Rid::from(rid)),
            None => writer,
        };
        let size = payload.len() as u64;
        if writer.write(pt, at, MediaTime::new(rtp, Frequency::NINETY_KHZ), payload).is_ok() {
            self.video_bytes += size;
        }
    }

    fn picture_in(&mut self, data: MediaData) {
        if data.mid != self.link.screen_mid() || !self.controls.watch() {
            return;
        }
        let user = self.shown.as_ref().map(|pictured| pictured.user_id.clone()).unwrap_or_default();
        let kind = self.shown.as_ref().map(|pictured| pictured.kind).unwrap_or_default();
        let key = data.is_keyframe();
        self.controls.video_stats.lock().unwrap().received(data.data.len(), Instant::now());
        let frame = Received { key, contiguous: data.contiguous, data: data.data, user, kind };
        if !self.video.picture(frame) {
            self.ask_keyframe();
        }
    }

    fn ask_keyframe(&mut self) {
        let now = Instant::now();
        if self.pli_at.is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(300)) {
            return;
        }
        self.pli_at = Some(now);
        let screen = self.link.screen_mid();
        if let Some(stream) = self.link.rtc.direct_api().stream_rx_by_mid(screen, None) {
            stream.request_keyframe(KeyframeRequestKind::Pli);
        }
    }

    fn encode(&mut self, frame: &[i16; FRAME_SAMPLES], tick: u64, voice: bool) {
        let mut packet = [0u8; 1275];
        let Some(len) = self.encoder.encode(frame, &mut packet) else {
            return;
        };
        // DTX is off, so even a frame of silence is a real packet in the speech mode and carries
        // the in-band copy of the frame before it. The canned CELT silence packet is not: a
        // receiver splices 5 ms of concealment of the old mode onto it, which is the pop. A
        // one- or two-byte packet would be DTX after all: send it as itself, not as that.
        let (level, voice) = if len > 2 { (dbov(frame), voice) } else { (-127, false) };
        self.write(&packet[..len], tick, level, voice);
    }

    /// The microphone's 20 ms, through the processing and the gate. Once the fade and the
    /// silence trail have gone, a muted microphone sends nothing.
    fn capture(&mut self, audio: &mut Audio) -> u8 {
        let controls = self.controls.clone();
        let quality = controls.quality();
        if quality.prefer_audio_application() != self.encoder_audio_app {
            if let Some(encoder) = Encoder::voice_quality(quality) {
                self.encoder = encoder;
                self.encoder_audio_app = quality.prefer_audio_application();
            }
        }
        self.encoder.set_bitrate(controls.bitrate());
        let muted = controls.muted.load(Ordering::Relaxed);
        let mut frame = [0i16; FRAME_SAMPLES];
        let (step, heard) = listen(&controls, audio, &mut self.processor, &mut self.gate, &mut frame);
        if let Some(speaking) = step.speaking {
            out::send(&MediaResponse::Speaking { user_id: self.user_id.clone(), speaking });
        }
        if !self.connected || (muted && step.send.is_none() && step.early.is_empty()) {
            return heard;
        }
        let early = step.early.len() as u64;
        for (n, frame) in step.early.iter().enumerate() {
            self.encode(frame, self.ticks.saturating_sub(early - n as u64), true);
        }
        match step.send {
            Some(Send::Frame(frame)) => self.encode(&frame, self.ticks, true),
            // The trail is what ends a spurt for the far decoder: five frames of real silence
            // in the encoder's mode, not the fixed CELT packet, so there is no mode switch.
            Some(Send::Silence) => self.encode(&[0i16; FRAME_SAMPLES], self.ticks, false),
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
            let mut stats = self.meter.second(self.slots.values_mut().map(|slot| slot.stream.counts()));
            let video = self.controls.video_stats.lock().unwrap().sample(Instant::now());
            if self.sending_share || self.sending_camera || self.watching {
                stats.video = Some(video);
            }
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

    /// The bound on the first connect. A call that was up and then lost its media waits for
    /// chatd to resume it; there is no wall-clock give-up here.
    fn overdue(&self) {
        if self.lost.is_some() {
            return;
        }
        if !self.connected && self.audio.is_none() && self.link.answered.elapsed() > CONNECT {
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
            self.seal_voice();
            self.open_voice();
            self.picture_step();
            // A connection str0m has given up on waits for the next one, or for the bound.
            let timeout = match self.drain() {
                Some(at) => at,
                None => {
                    self.fell();
                    next
                }
            };
            let now = Instant::now();
            match next_step(now, next, !self.packets.is_empty()) {
                Next::Tick => {
                    let due = next + TICK;
                    next = after(next, now);
                    if next != due {
                        // The machine slept or stalled: do not lay on the link what this loop
                        // did not read.
                        self.heard += now.saturating_duration_since(due);
                    }
                    self.tick();
                    self.watch_link(now);
                    if self.ticks.is_multiple_of(SECOND) {
                        self.overdue();
                    }
                    continue;
                }
                Next::Packet => {
                    if let Some(packet) = self.packets.pop_front() {
                        if !self.input(packet) {
                            tracing::warn!("mediad: str0m refused a packet");
                        }
                    }
                    continue;
                }
                Next::Wait => {}
            }
            let wait = timeout.min(next).saturating_duration_since(now).max(Duration::from_millis(1));
            if let Err(err) = self.poll.poll(&mut events, Some(wait)) {
                if err.kind() != ErrorKind::Interrupted {
                    tracing::warn!("mediad: waiting on the call's sockets: {err}");
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
        for (mid, slot) in &mut self.slots {
            slot.stream.log_playout(&format_args!("slot {mid}"), true);
        }
        if let Some(audio) = &self.audio {
            media::sign_off(audio);
        }
        if let Some(devices) = &mut self.devices {
            devices.stop();
        }
        std::mem::replace(&mut self.video, Video::new(self.controls.clone())).stop();
    }
}

/// Opens the sound devices on a thread of their own, from the moment the server's answer is in
/// until ICE and DTLS have finished, which take their time on any network: the call used to open
/// them only once media flowed. The microphone is opened stopped, so nothing is captured and no
/// recording indicator shows before the call is up. The noise-suppression model is built here
/// too. `None` when the thread did not start: the devices then open when media flows.
fn open_beside_connecting(controls: &Arc<Controls>) -> Option<std::sync::mpsc::Receiver<Result<Audio, crate::audio::AudioError>>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let controls = controls.clone();
    let spawned = std::thread::Builder::new().name("devices-open".to_owned()).spawn(move || {
        let started = Instant::now();
        let (input, output) = controls.take_chosen();
        let opened = Audio::open_waiting(input, output);
        tracing::debug!(elapsed_ms = started.elapsed().as_millis() as u64, ok = opened.is_ok(), "mediad: devices opened");
        stages::mark("devices_open");
        let _ = sender.send(opened);
        crate::process::warm_isolation(controls.processing());
    });
    spawned.ok().map(|_| receiver)
}

/// Runs the call until chatd hangs up. A failure does not return (`out::fail` exits).
pub fn run(link: Link, user_id: String, controls: Arc<Controls>) {
    // A phone's audio session, for as long as the call runs. Nothing on a desktop.
    let _audio_session = crate::platform::call_audio();
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
    let quality = controls.quality();
    let (Some(encoder), Ok(poll)) = (Encoder::voice_quality(quality), Poll::new()) else {
        tracing::warn!("mediad: the Opus encoder or the poller could not be made");
        out::fail(CallFailure::Engine, ENGINE);
    };
    let mut call = Call {
        controls: controls.clone(),
        user_id,
        link,
        poll,
        next_token: 1,
        packets: VecDeque::new(),
        unsealed: VecDeque::new(),
        unopened: VecDeque::new(),
        connected: false,
        announced: false,
        lost: None,
        heard: Instant::now(),
        silenced: false,
        audio: None,
        opening: open_beside_connecting(&controls),
        devices: None,
        microphone_unasked,
        encoder,
        encoder_audio_app: quality.prefer_audio_application(),
        gate: Gate::default(),
        slots: HashMap::new(),
        ticks: 0,
        levels: (0, 0),
        cue: None,
        processor: Processor::new(),
        limiter: Limiter::default(),
        meter: Meter::own_path(CallPath::Unknown),
        video: Video::new(controls.clone()),
        estimate: None,
        rates: Rates::default(),
        uplink: video::Uplink::default(),
        video_bytes: 0,
        desired: 0,
        padding_dropped: None,
        sending_share: false,
        sending_camera: false,
        offered: None,
        pli_at: None,
        shown: None,
        watching: false,
        share_noted: false,
        camera_noted: false,
    };
    if let Err(err) = call.register() {
        tracing::warn!("mediad: the call's socket: {err}");
        out::fail(CallFailure::Engine, ENGINE);
    }
    call.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_video_counts_frames_at_markers_and_excludes_padding_and_audio() {
        let packet = |first, second| {
            let mut packet = vec![0u8; 20];
            packet[0] = first;
            packet[1] = second;
            packet
        };
        assert_eq!(video_packet(&packet(0x80, H264_PT)), Some(false));
        assert_eq!(video_packet(&packet(0x80, H264_PT | 0x80)), Some(true));
        assert_eq!(video_packet(&packet(0x80, H264_RTX_PT | 0x80)), Some(false));
        assert_eq!(video_packet(&packet(0xa0, H264_RTX_PT)), None);
        assert_eq!(video_packet(&packet(0x80, 111)), None);
        assert_eq!(video_packet(&packet(0x80, 200)), None);
        assert_eq!(video_packet(&[0x80, H264_PT]), None);
    }

    fn slot(mid: &str, user: &str) -> TinySlot {
        TinySlot { mid: mid.into(), user_id: user.into() }
    }

    #[test]
    fn a_due_tick_goes_ahead_of_any_packets_and_a_stall_is_not_replayed() {
        let start = Instant::now();
        assert_eq!(next_step(start + TICK, start + TICK, true), Next::Tick, "packets queued do not hold the tick back");
        assert_eq!(next_step(start, start + TICK, true), Next::Packet);
        assert_eq!(next_step(start, start + TICK, false), Next::Wait);
        assert_eq!(after(start, start + TICK), start + TICK, "on time");
        assert_eq!(after(start, start + 5 * TICK), start + TICK, "a short slip catches up one tick at a time");
        let late = start + 7 * TICK;
        assert_eq!(after(start, late), late + TICK, "past the bound the missed ticks are gone");
    }

    #[test]
    fn egress_loss_counts_only_when_no_padding_was_dropped_in_its_span() {
        let now = Instant::now();
        let ago = |ms: u64| now - Duration::from_millis(ms);
        assert_eq!(egress_loss(Some(0.5), Some(now), now), None, "dropping it now");
        assert_eq!(egress_loss(Some(0.5), Some(ago(900)), now), None, "video just started: this second has some in it");
        assert_eq!(egress_loss(Some(0.1), Some(ago(1100)), now), Some(0.1), "a second of real sends");
        assert_eq!(egress_loss(Some(0.1), None, now), Some(0.1), "none ever dropped");
        assert_eq!(egress_loss(None, None, now), None);
    }

    #[test]
    fn a_lost_audio_packet_is_not_held_for_reordering() {
        let config = rtc_config();
        assert_eq!(config.reordering_size_audio(), 0);
        assert_eq!(config.reordering_timeout_audio(), Some(Duration::from_millis(40)));
    }

    /// How long str0m takes to give up on a path that never answers, on its own clock.
    #[test]
    fn a_path_that_answers_nothing_is_given_up_in_seconds() {
        let offer = Offer::new(&spec(1)).unwrap();
        let mut peer = rtc_config().build(Instant::now());
        peer.add_local_candidate(Candidate::host("127.0.0.1:9".parse().unwrap(), "udp").unwrap());
        let incoming = str0m::change::SdpOffer::from_sdp_string(offer.sdp()).expect("offer");
        let answer = peer.sdp_api().accept_offer(incoming).expect("answer");
        let mut link = offer.accept(&answer.to_sdp_string()).expect("accept");
        let began = Instant::now();
        let mut now = began;
        loop {
            match link.rtc.poll_output().expect("output") {
                Output::Timeout(at) => {
                    now = at.max(now + Duration::from_millis(1));
                    link.rtc.handle_input(Input::Timeout(now)).expect("a timeout");
                }
                Output::Event(Event::IceConnectionStateChange(IceConnectionState::Disconnected)) => break,
                _ => {}
            }
            assert!(now - began < Duration::from_secs(30), "still trying after {:?}", now - began);
        }
        let taken = now - began;
        assert!((Duration::from_secs(4)..=Duration::from_secs(7)).contains(&taken), "gave up after {taken:?}");
    }

    #[test]
    fn a_timestamp_gap_is_a_missing_frame() {
        use crate::jitter::{Jitter, Pop};
        assert_eq!(frame_sequence(0), 0);
        assert_eq!(frame_sequence(960), 1);
        assert_eq!(frame_sequence(960 * 3), 3);
        let mut jitter = Jitter::default();
        for (stamp, byte) in [(0u64, 1u8), (960, 2), (960 * 3, 4)] {
            jitter.insert(frame_sequence(stamp), vec![byte]);
        }
        assert!(matches!(jitter.pop(), Pop::Packet(_)));
        assert!(matches!(jitter.pop(), Pop::Packet(_)));
        assert_eq!(jitter.pop(), Pop::Lost(1));
        assert_eq!(jitter.next(), Some(&[4u8][..]));
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
        TinyOffer { user_id: "me".into(), host: "127.0.0.1".into(), slots, muted: false, deafened: false, e2ee: None }
    }

    /// One microphone and `slots` receive slots, Opus on every audio line, then the three
    /// video lines the SFU accepts: a simulcast camera and a share, both inactive until
    /// asked, and one receive slot for the picture the room forwards.
    #[test]
    fn the_offer_is_one_microphone_then_a_receive_slot_for_each_other_seat() {
        let offer = Offer::new(&spec(11)).unwrap();
        let sdp = offer.sdp();
        assert_eq!(sdp.matches("m=audio").count(), 12, "{sdp}");
        assert_eq!(sdp.matches("a=sendonly").count(), 1);
        assert_eq!(sdp.matches("a=recvonly").count(), 12, "eleven voices and one picture");
        assert_eq!(sdp.matches("a=inactive").count(), 2, "camera and share stay off");
        assert!(!sdp.contains("m=application"));
        let video = video_sections(sdp);
        assert_eq!(video.len(), 3, "{sdp}");
        assert!(video[0].contains("a=inactive") && video[0].contains("a=rid:h") && video[0].contains("a=rid:l"), "camera simulcast, off: {}", video[0]);
        assert!(video[0].contains("a=simulcast:"), "camera names its layers: {}", video[0]);
        assert!(video[1].contains("a=inactive") && !video[1].contains("a=rid:"), "share is one layer and off: {}", video[1]);
        assert!(video[2].contains("a=recvonly") && !video[2].contains("a=rid:"), "one picture in: {}", video[2]);
        for section in &video {
            assert!(section.to_ascii_lowercase().contains("profile-level-id=42e033"), "constrained baseline, level 5.1: {section}");
            assert!(section.contains("packetization-mode=1"), "{section}");
            assert!(section.contains("H264/90000"), "{section}");
        }
        let audio: Vec<&str> = sdp.lines().filter(|line| line.starts_with("m=audio")).collect();
        assert!(audio.iter().all(|line| !line.to_ascii_lowercase().contains("h264")), "{audio:?}");
        assert!(sdp.contains("ssrc-audio-level"), "the RFC 6464 extension is offered");
        assert!(sdp.contains(" udp ") || sdp.contains(" UDP "), "a UDP host candidate");
        assert!(sdp.contains("tcptype active"), "and an active ICE-TCP one");
        assert!(sdp.contains("127.0.0.1"), "on the address that reaches the server");
        assert!(sdp.len() < 32 * 1024, "under the server's cap");
        assert!(!sdp.contains("ice-lite"), "we are the full ICE agent");
    }

    /// Video m-line bodies, in offer order.
    fn video_sections(sdp: &str) -> Vec<String> {
        let mut sections = Vec::new();
        let mut current: Option<String> = None;
        for line in sdp.lines() {
            if line.starts_with("m=") {
                if let Some(done) = current.take() {
                    sections.push(done);
                }
                if line.starts_with("m=video") {
                    current = Some(String::new());
                }
            }
            if let Some(section) = current.as_mut() {
                section.push_str(line);
                section.push('\n');
            }
        }
        if let Some(done) = current {
            sections.push(done);
        }
        sections
    }

    #[test]
    fn an_answer_that_is_not_sdp_is_refused() {
        let offer = Offer::new(&spec(1)).unwrap();
        assert!(offer.accept("not sdp").is_err());
    }

    /// A peer that has taken our offer, so a later offer has an answer to accept.
    fn linked(slots: u16) -> (Link, Rtc) {
        let offer = Offer::new(&spec(slots)).unwrap();
        let mut peer = rtc_config().build(Instant::now());
        let incoming = str0m::change::SdpOffer::from_sdp_string(offer.sdp()).expect("offer");
        let answer = peer.sdp_api().accept_offer(incoming).expect("answer");
        let link = offer.accept(&answer.to_sdp_string()).expect("accept");
        (link, peer)
    }

    /// Moves packets between two connections on a clock of the test's, a millisecond at a time.
    /// `sent` hears each packet the first one sends, and `events` what it reports.
    struct Wire {
        now: Instant,
        ours: SocketAddr,
        theirs: SocketAddr,
        sent: Vec<(Instant, Vec<u8>)>,
        events: Vec<String>,
        estimates: Vec<u64>,
        /// As [`Call::transmit`] does with no video to send.
        no_padding: bool,
        pictures: Vec<MediaData>,
    }

    impl Wire {
        fn step(&mut self, link: &mut Link, peer: &mut Rtc) {
            self.now += Duration::from_millis(1);
            let now = self.now;
            link.rtc.handle_input(Input::Timeout(now)).unwrap();
            peer.handle_input(Input::Timeout(now)).unwrap();
            let mut to_peer = Vec::new();
            let mut to_link = Vec::new();
            loop {
                match link.rtc.poll_output().unwrap() {
                    Output::Timeout(_) => break,
                    Output::Transmit(transmit) => {
                        if self.no_padding && is_padding(&transmit.contents) {
                            continue;
                        }
                        self.sent.push((now, transmit.contents.to_vec()));
                        to_peer.push(transmit.contents.to_vec());
                    }
                    Output::Event(Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. })) => self.estimates.push(estimate.as_u64()),
                    Output::Event(event) => self.events.push(format!("{event:?}")),
                }
            }
            loop {
                match peer.poll_output().unwrap() {
                    Output::Timeout(_) => break,
                    Output::Transmit(transmit) => to_link.push(transmit.contents.to_vec()),
                    Output::Event(Event::MediaData(data)) if data.mid == link.share_mid() => self.pictures.push(data),
                    Output::Event(_) => {}
                }
            }
            for packet in to_peer {
                let contents = packet.as_slice().try_into().unwrap();
                let receive = Receive { proto: Protocol::Udp, source: self.ours, destination: self.theirs, contents };
                peer.handle_input(Input::Receive(now, receive)).unwrap();
            }
            for packet in to_link {
                let contents = packet.as_slice().try_into().unwrap();
                let receive = Receive { proto: Protocol::Udp, source: self.theirs, destination: self.ours, contents };
                link.rtc.handle_input(Input::Receive(now, receive)).unwrap();
            }
        }

        fn run(&mut self, link: &mut Link, peer: &mut Rtc, for_ms: u32) {
            for _ in 0..for_ms {
                self.step(link, peer);
            }
        }

        /// RTP packets with this payload type sent since `from`.
        fn rtp(&self, pt: u8, from: Instant) -> Vec<Instant> {
            let is = |packet: &Vec<u8>| packet.len() > 12 && packet[0] >> 6 == 2 && packet[1] & 0x7f == pt;
            self.sent.iter().filter(|(at, packet)| *at >= from && is(packet)).map(|(at, _)| *at).collect()
        }
    }

    /// A connection and a peer standing in for the SFU, connected.
    fn connected() -> (Link, Rtc, Wire) {
        let theirs = SocketAddr::from(([127, 0, 0, 1], 40_000));
        let offer = Offer::new(&TinyOffer { user_id: "me".into(), host: "127.0.0.1".into(), slots: 1, muted: false, deafened: false, e2ee: None }).unwrap();
        let ours = offer.link.local_udp.unwrap();
        let mut peer = rtc_config().build(Instant::now());
        peer.add_local_candidate(Candidate::host(theirs, "udp").unwrap());
        let incoming = str0m::change::SdpOffer::from_sdp_string(offer.sdp()).unwrap();
        let answer = peer.sdp_api().accept_offer(incoming).unwrap();
        let mut link = offer.accept(&answer.to_sdp_string()).unwrap();
        let mut wire = Wire { now: Instant::now(), ours, theirs, sent: Vec::new(), events: Vec::new(), estimates: Vec::new(), no_padding: false, pictures: Vec::new() };
        for _ in 0..3000 {
            wire.step(&mut link, &mut peer);
            if wire.events.iter().any(|event| event == "Connected") {
                break;
            }
        }
        assert!(wire.events.iter().any(|event| event == "Connected"), "{:?}", wire.events);
        (link, peer, wire)
    }

    /// The same, with the share turned on.
    fn sharing() -> (Link, Rtc, Wire) {
        let (mut link, mut peer, mut wire) = connected();
        let sdp = link.offer_send(true, true).unwrap();
        let answer = peer.sdp_api().accept_offer(str0m::change::SdpOffer::from_sdp_string(&sdp).unwrap()).unwrap();
        link.take_answer(&answer.to_sdp_string()).unwrap();
        wire.run(&mut link, &mut peer, 50);
        (link, peer, wire)
    }

    fn annex_b(len: usize, name: u8) -> Vec<u8> {
        let mut frame = vec![0, 0, 0, 1, name];
        frame.resize(len, 0x55);
        frame
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "captures the interactive Windows desktop and needs the OpenH264 binary"]
    fn a_real_windows_screen_survives_webrtc_and_is_painted() {
        use crate::picture::{Ask, Incoming, Outgoing, Tag};
        let (mut link, mut peer, mut wire) = sharing();
        let share = link.share_mid();
        let pt = link.rtc.writer(share).unwrap().payload_params().next().unwrap().pt();
        let mut source = Outgoing::default();
        let mut screen = Incoming::default();
        let began = Instant::now();
        let capture = crate::picture::capture_screen().unwrap();
        eprintln!("capture {}x{} rgb max {}", capture.width, capture.height, capture.pixels.chunks_exact(4).flat_map(|pixel| pixel[..3].iter()).max().unwrap());
        assert!(capture.pixels.chunks_exact(4).any(|pixel| pixel[..3].iter().any(|value| *value > 32)), "the interactive desktop's first capture must not be black");
        if let Some(folder) = std::env::var_os("TINYCLIENT_VIDEO_PROOF_FOLDER") {
            std::fs::create_dir_all(&folder).unwrap();
            let mut rgba = capture.pixels;
            for pixel in rgba.chunks_exact_mut(4) {
                if capture.bgra { pixel.swap(0, 2); }
                pixel[3] = 255;
            }
            let image = image::RgbaImage::from_raw(capture.width, capture.height, rgba).unwrap();
            image::DynamicImage::ImageRgba8(image).to_rgb8().save(std::path::PathBuf::from(folder).join("captured-screen.jpg")).unwrap();
        }
        let mut encoded_bytes = 0;
        let mut frames = 0;
        for n in 0..60 {
            let ask = Ask { key: n == 0 || n == 30, bps: 8_000_000, at: Duration::from_millis(n * 17), fps: 60. };
            let coded = loop {
                match source.share(ask) {
                    Ok(coded) => break coded,
                    Err(words) if words == crate::picture::CODEC_LATER && began.elapsed() < Duration::from_secs(30) => std::thread::sleep(Duration::from_millis(20)),
                    Err(words) => panic!("capture/encode: {words}"),
                }
            };
            if !coded.bytes.is_empty() {
                encoded_bytes += coded.bytes.len();
                frames += 1;
                link.rtc.writer(share).unwrap().write(pt, wire.now, MediaTime::new(n * 1530, Frequency::NINETY_KHZ), coded.bytes).unwrap();
            }
            wire.run(&mut link, &mut peer, 17);
        }
        wire.run(&mut link, &mut peer, 5000);
        assert!(!wire.pictures.is_empty(), "video must cross DTLS-SRTP and depacketize");
        let received = wire.pictures.len();
        let mut images = Vec::new();
        for data in wire.pictures {
            screen.decode(&data.data, Some(Tag { user: "proof".into(), kind: protocol::PictureKind::Share })).expect("every received frame decodes");
            images.extend(screen.painted());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while images.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            images.extend(screen.painted());
        }
        let image = &images.last().expect("a real received image must be produced").1;
        let decoded = image::ImageReader::open(&image.path).unwrap().decode().unwrap().to_rgb8();
        if let Some(folder) = std::env::var_os("TINYCLIENT_VIDEO_PROOF_FOLDER") {
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::copy(&image.path, std::path::PathBuf::from(folder).join("received-screen.jpg")).unwrap();
        }
        assert!(decoded.pixels().any(|pixel| pixel.0.iter().any(|channel| *channel > 32)), "the received desktop must not be black");
        eprintln!("screen proof: {frames} encoded, {received} received, {}x{}, {encoded_bytes} encoded bytes, elapsed {:?}", image.width, image.height, began.elapsed());
        screen.forget();
    }

    /// Without the pacer a keyframe left as one burst, every packet in one millisecond, in front
    /// of whatever audio came next. With it the keyframe spreads out, audio goes straight past,
    /// and the feedback gives an estimate for the encoders.
    #[test]
    fn a_keyframe_is_paced_audio_is_not_and_feedback_gives_an_estimate() {
        let (mut link, mut peer, mut wire) = sharing();
        let share = link.share_mid();
        let video_pt = link.rtc.writer(share).unwrap().payload_params().next().unwrap().pt();
        let microphone = link.microphone_mid();
        let audio_pt = link.rtc.writer(microphone).unwrap().payload_params().next().unwrap().pt();
        let start = wire.now;
        let writer = link.rtc.writer(share).unwrap();
        writer.write(video_pt, start, MediaTime::new(0, Frequency::NINETY_KHZ), annex_b(150_000, 0x65)).unwrap();
        wire.step(&mut link, &mut peer);

        let spoke = wire.now;
        let writer = link.rtc.writer(microphone).unwrap();
        writer.write(audio_pt, spoke, MediaTime::new(960, Frequency::FORTY_EIGHT_KHZ), vec![0x78; 80]).unwrap();
        wire.run(&mut link, &mut peer, 2);
        let audio = wire.rtp(*audio_pt, spoke);
        assert_eq!(audio.len(), 1, "audio does not wait for the keyframe");
        assert!(audio[0] <= spoke + Duration::from_millis(2));

        wire.run(&mut link, &mut peer, 3000);
        let video = wire.rtp(*video_pt, start);
        assert!(video.len() > 100, "all of it went: {}", video.len());
        let span = *video.last().unwrap() - video[0];
        assert!(span > Duration::from_millis(100), "the keyframe was spread out: {span:?}");
        assert!(video.iter().filter(|at| **at == video[0]).count() < video.len() / 4, "and not one burst");
        assert!(!wire.estimates.is_empty(), "the feedback gave an estimate");
    }

    fn talk(link: &mut Link, peer: &mut Rtc, wire: &mut Wire, ms: u64) {
        let microphone = link.microphone_mid();
        let pt = link.rtc.writer(microphone).unwrap().payload_params().next().unwrap().pt();
        for n in 0..ms / 20 {
            let writer = link.rtc.writer(microphone).unwrap();
            writer.write(pt, wire.now, MediaTime::new(n * 960, Frequency::FORTY_EIGHT_KHZ), vec![0x78; 40]).unwrap();
            wire.run(link, peer, 20);
        }
    }

    fn padding_after(wire: &Wire, from: Instant) -> usize {
        wire.sent.iter().filter(|(at, packet)| *at >= from && is_padding(packet)).map(|(_, packet)| packet.len()).sum()
    }

    /// str0m pads an inactive line. This is what makes that visible, and what stops it.
    #[test]
    fn a_voice_call_sends_no_padding() {
        let (mut link, mut peer, mut wire) = connected();
        talk(&mut link, &mut peer, &mut wire, 3000);
        assert!(padding_after(&wire, wire.now - Duration::from_secs(1)) > 1000, "str0m still pads, or this test proves nothing");

        let (mut link, mut peer, mut wire) = connected();
        wire.no_padding = true;
        let start = wire.now;
        talk(&mut link, &mut peer, &mut wire, 3000);
        assert_eq!(padding_after(&wire, start), 0);
        assert!(!wire.rtp(*link.rtc.writer(link.microphone).unwrap().payload_params().next().unwrap().pt(), start).is_empty());
    }

    #[test]
    fn only_rtx_rtp_counts_as_padding() {
        assert!(is_padding(&[0xa0, H264_RTX_PT, 0, 1]));
        assert!(is_padding(&[0xa0, H264_RTX_PT | 0x80, 0, 1]));
        assert!(!is_padding(&[0x80, H264_PT, 0, 1]), "video");
        assert!(!is_padding(&[0x81, 205, 0, 1]), "RTCP");
        assert!(!is_padding(&[0x00, H264_RTX_PT]), "STUN");
        assert!(!is_padding(&[22, 254]), "DTLS");
        assert!(!is_padding(&[]));
    }

    /// The padding thrown away reads as loss to the estimator, so video starts from a fresh one.
    #[test]
    fn video_after_a_quiet_voice_call_starts_from_a_fresh_estimate() {
        let (mut link, mut peer, mut wire) = connected();
        wire.no_padding = true;
        talk(&mut link, &mut peer, &mut wire, 4000);
        let sdp = link.offer_send(true, true).unwrap();
        let answer = peer.sdp_api().accept_offer(str0m::change::SdpOffer::from_sdp_string(&sdp).unwrap()).unwrap();
        link.take_answer(&answer.to_sdp_string()).unwrap();
        wire.no_padding = false;
        link.rtc.bwe().reset(Bitrate::bps(START_BPS));
        link.rtc.bwe().set_desired_bitrate(Bitrate::bps(2_000_000));
        wire.estimates.clear();
        let share = link.share_mid();
        let pt = link.rtc.writer(share).unwrap().payload_params().next().unwrap().pt();
        for n in 0..45u64 {
            let name = if n == 0 { 0x65 } else { 0x41 };
            let writer = link.rtc.writer(share).unwrap();
            writer.write(pt, wire.now, MediaTime::new(n * 6000, Frequency::NINETY_KHZ), annex_b(6000, name)).unwrap();
            wire.run(&mut link, &mut peer, 66);
        }
        let last = wire.estimates.last().copied().unwrap_or(0);
        assert!(last >= 500_000, "{:?}", wire.estimates);
    }

    /// The share stays inactive until its answer is in. An empty answer is a refusal
    /// (`share_taken`): the m-line goes back, and the next offer can start it again.
    #[test]
    fn a_share_starts_only_once_its_answer_is_in_and_a_refusal_puts_it_back() {
        let (mut link, mut peer) = linked(1);
        assert_eq!(link.direction(true), Direction::Inactive);
        assert_eq!(link.direction(false), Direction::Inactive);
        let sdp = link.offer_send(true, true).unwrap();
        let sections = video_sections(&sdp);
        assert!(sections[1].contains("a=sendonly"), "the share is the second video line: {sdp}");
        assert!(sections[0].contains("a=inactive"), "the camera is untouched");
        assert!(link.take_answer("").is_err(), "an empty answer is a refusal");
        assert_eq!(link.direction(true), Direction::Inactive, "the refusal puts the share back");
        let sdp = link.offer_send(true, true).unwrap();
        let incoming = str0m::change::SdpOffer::from_sdp_string(&sdp).unwrap();
        let answer = peer.sdp_api().accept_offer(incoming).unwrap();
        link.take_answer(&answer.to_sdp_string()).unwrap();
        assert_eq!(link.direction(true), Direction::SendOnly);
        assert_eq!(link.direction(false), Direction::Inactive);
        assert!(link.is_picture(link.screen_mid()));
        assert!(!link.is_picture(link.microphone_mid()));
    }
}
