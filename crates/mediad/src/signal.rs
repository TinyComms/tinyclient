//! Signal 1:1 and group call media transport: pure-Rust WebRTC through `str0m` (ICE, DTLS-SRTP,
//! Opus 48 kHz audio, H.264 constrained baseline video).
//!
//! Why pure-Rust `str0m` instead of linking C++ RingRTC:
//! - tinyclient ships across 5 platforms (Windows, macOS, Linux, Android, iOS) with zero native
//!   C++ WebRTC build toolchain. Linking Signal's RingRTC fork would pull a ~1 GB pinned Chromium
//!   WebRTC C++ build into every platform CI job and break on every Signal release bump.
//! - `mediad` already carries a pure-Rust `str0m` + RustCrypto DTLS-SRTP + Opus + H.264
//!   (hardware Media Foundation / OpenH264) stack with AEC3/NS/AGC2 microphone processing.
//! - Signal call signalling (`OfferMessage`, `AnswerMessage`, `IceUpdateMessage`, `HangupMessage`,
//!   `BusyMessage`, `OpaqueMessage`) travels end-to-end encrypted inside Signal's Double Ratchet /
//!   Sealed Sender channel, authenticating the DTLS fingerprints exchanged in SDP alongside the
//!   peer's Signal Safety Number.

use crate::audio::{level, Audio};
use crate::cue::Playing;
use crate::hear::{Ear, Limiter};
use crate::media::{self, announce, finish_cue, listen, mixdown, Devices, Gate, Send as MicSend, Stream, MICROPHONE_DENIED};
use crate::opus::Encoder;
use crate::permission::{microphone, Microphone};
use crate::process::Processor;
use crate::rtp::SILENCE;
use crate::session::Controls;
use crate::stats::Meter;
use crate::tiny::dbov;
use crate::video::{self, Rates, Received, Track, Video, Wants, Word};
use crate::{out, rt, FRAME, FRAME_SAMPLES};
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use protocol::{CallFailure, CallPath, MediaResponse, PictureKind, SignalAccept, SignalOffer, SignalRelay};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

const TICK: Duration = Duration::from_millis(20);
const SECOND: u64 = 50;
const VOICE: u8 = 4;
const H264_PROFILE: u32 = 0x42e033;
const H264_PT: u8 = 108;
const H264_RTX_PT: u8 = 109;
const START_BPS: u64 = 1_000_000;
const STUN_MAX_RTO: Duration = Duration::from_secs(1);
const STUN_RETRANSMITS: usize = 8;
const ENGINE: &str = "The Signal call engine could not start. Try again.";

fn dtls_cert() -> Option<str0m::crypto::dtls::DtlsCert> {
    static CERT: std::sync::OnceLock<Option<str0m::crypto::dtls::DtlsCert>> = std::sync::OnceLock::new();
    CERT.get_or_init(|| str0m::crypto::from_feature_flags().dtls_provider.generate_certificate()).clone()
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

/// Chooses the local IP that routes toward `host`, defaulting to loopback if unroutable.
fn local_ip(host: &str) -> IpAddr {
    let target = if host.is_empty() {
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443))
    } else if let Ok(addr) = host.parse::<SocketAddr>() {
        addr
    } else if let Ok(ip) = host.parse::<IpAddr>() {
        SocketAddr::new(ip, 443)
    } else {
        (host, 443)
            .to_socket_addrs()
            .ok()
            .and_then(|mut it| it.find(|a| a.is_ipv4()))
            .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, 443)))
    };
    std::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .and_then(|s| {
            s.connect(target)?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// Extracts a socket address from a TURN URL (`turn:host:port?transport=udp` or `turns:host:port`).
fn relay_addr(relay: &SignalRelay) -> Option<SocketAddr> {
    for url in &relay.urls {
        let rest = url
            .strip_prefix("turns:")
            .or_else(|| url.strip_prefix("turn:"))
            .or_else(|| url.strip_prefix("stun:"))?;
        let host_port = rest.split('?').next().unwrap_or(rest);
        if let Ok(addr) = host_port.parse::<SocketAddr>() {
            return Some(addr);
        }
        if let Ok(mut addrs) = host_port.to_socket_addrs() {
            if let Some(addr) = addrs.find(|a| a.is_ipv4()) {
                return Some(addr);
            }
        }
    }
    None
}

/// Parses an SDP ICE candidate string (`candidate:...` or `a=candidate:...`).
pub fn parse_candidate(line: &str) -> Result<Candidate, String> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix("a=").unwrap_or(trimmed);
    Candidate::from_sdp_string(body).map_err(|err| format!("invalid ICE candidate: {err:?}"))
}

/// Extracts the first audio and video `Mid`s from an SDP document's `m=` / `a=mid:` sections.
fn sdp_mids(sdp: &str) -> (Option<Mid>, Option<Mid>) {
    let mut section: Option<&str> = None;
    let mut audio: Option<Mid> = None;
    let mut video: Option<Mid> = None;
    for line in sdp.lines() {
        let line = line.trim();
        if line.starts_with("m=audio ") {
            section = Some("audio");
        } else if line.starts_with("m=video ") {
            section = Some("video");
        } else if line.starts_with("m=") {
            section = None;
        } else if let Some(mid) = line.strip_prefix("a=mid:") {
            match section {
                Some("audio") if audio.is_none() => audio = Some(Mid::from(mid)),
                Some("video") if video.is_none() => video = Some(Mid::from(mid)),
                _ => {}
            }
        }
    }
    (audio, video)
}

/// Adds local host or relay-only ICE candidates to `rtc`, returning the bound UDP socket,
/// the local destination address for incoming packets, and the SDP candidate lines to trickle.
fn gather_candidates(
    rtc: &mut Rtc,
    host: &str,
    relay_only: bool,
    relays: &[SignalRelay],
) -> io::Result<(Option<UdpSocket>, Option<SocketAddr>, Vec<String>)> {
    let bound = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
    let port = bound.local_addr()?.port();
    let mut lines = Vec::new();
    let local_addr;

    if relay_only {
        // Privacy P7 / non-contact relay mode: never expose the local host IP in SDP or ICE.
        let mut relay_local = None;
        for relay in relays {
            if let Some(addr) = relay_addr(relay) {
                if let Ok(cand) = Candidate::relayed(addr, SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), Protocol::Udp) {
                    lines.push(cand.to_sdp_string());
                    rtc.add_local_candidate(cand);
                    relay_local.get_or_insert(addr);
                }
            }
        }
        if relay_local.is_none() {
            // Keep a loopback relay placeholder when no TURN resolved offline,
            // while still guaranteeing zero host candidates are published.
            let fallback = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            if let Ok(cand) = Candidate::relayed(fallback, SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), Protocol::Udp) {
                lines.push(cand.to_sdp_string());
                rtc.add_local_candidate(cand);
                relay_local = Some(fallback);
            }
        }
        local_addr = relay_local.or(Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port))));
    } else {
        let ip = local_ip(host);
        let addr = SocketAddr::new(ip, port);
        let cand = Candidate::host(addr, "udp").map_err(|err| io::Error::other(format!("candidate: {err:?}")))?;
        lines.push(cand.to_sdp_string());
        rtc.add_local_candidate(cand);
        local_addr = Some(addr);
    }

    Ok((Some(bound), local_addr, lines))
}

fn frame_sequence(timestamp: u64) -> u16 {
    (timestamp / FRAME as u64) as u16
}

/// One live Signal WebRTC session (str0m + socket + audio/video mids).
pub struct Link {
    rtc: Rtc,
    udp: Option<UdpSocket>,
    local_udp: Option<SocketAddr>,
    audio: Mid,
    video: Mid,
    relay_only: bool,
    video_initial: bool,
    pending_reneg: Option<SdpPendingOffer>,
    local_candidates: Vec<String>,
}

/// Outgoing Signal call offer waiting for the remote peer's SDP answer.
pub struct Offer {
    link: Link,
    pending: SdpPendingOffer,
    sdp: String,
}

/// Incoming Signal call answer ready to start media immediately.
pub struct Answer {
    link: Link,
    sdp: String,
}

impl Offer {
    pub fn new(spec: &SignalOffer) -> io::Result<Self> {
        let mut rtc = rtc_config().build(Instant::now());
        let (udp, local_udp, local_candidates) = gather_candidates(&mut rtc, &spec.host, spec.relay_only, &spec.relays)?;
        let mut change = rtc.sdp_api();
        let audio = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let video_dir = if spec.video { Direction::SendRecv } else { Direction::RecvOnly };
        let video = change.add_media(MediaKind::Video, video_dir, None, None, None);
        let (offer, pending) = change.apply().ok_or_else(|| io::Error::other("signal offer produced no SDP"))?;
        let sdp = offer.to_sdp_string();
        let link = Link {
            rtc,
            udp,
            local_udp,
            audio,
            video,
            relay_only: spec.relay_only,
            video_initial: spec.video,
            pending_reneg: None,
            local_candidates,
        };
        Ok(Self { link, pending, sdp })
    }

    pub fn sdp(&self) -> &str {
        &self.sdp
    }

    pub fn local_candidates(&self) -> &[String] {
        &self.link.local_candidates
    }

    pub fn add_remote_candidate(&mut self, candidate: &str) -> Result<(), String> {
        let cand = parse_candidate(candidate)?;
        self.link.rtc.add_remote_candidate(cand);
        Ok(())
    }

    pub fn accept(mut self, answer_sdp: &str) -> Result<Link, String> {
        let answer = SdpAnswer::from_sdp_string(answer_sdp).map_err(|err| format!("{err:?}"))?;
        self.link.rtc.sdp_api().accept_answer(self.pending, answer).map_err(|err| format!("{err:?}"))?;
        Ok(self.link)
    }
}

impl Answer {
    pub fn from_offer(spec: &SignalAccept) -> Result<Self, String> {
        let mut rtc = rtc_config().build(Instant::now());
        let (udp, local_udp, local_candidates) = gather_candidates(&mut rtc, &spec.host, spec.relay_only, &spec.relays)
            .map_err(|err| err.to_string())?;
        let offer = SdpOffer::from_sdp_string(&spec.offer_sdp).map_err(|err| format!("{err:?}"))?;
        let answer = rtc.sdp_api().accept_offer(offer).map_err(|err| format!("{err:?}"))?;
        let sdp = answer.to_sdp_string();
        let (audio, video) = sdp_mids(&sdp);
        let audio = audio.ok_or_else(|| "offer contained no audio m-line".to_owned())?;
        let video = video.unwrap_or(audio);
        let link = Link {
            rtc,
            udp,
            local_udp,
            audio,
            video,
            relay_only: spec.relay_only,
            video_initial: spec.video,
            pending_reneg: None,
            local_candidates,
        };
        Ok(Self { link, sdp })
    }

    pub fn sdp(&self) -> &str {
        &self.sdp
    }

    pub fn local_candidates(&self) -> &[String] {
        &self.link.local_candidates
    }

    pub fn into_link(self) -> Link {
        self.link
    }
}

impl Link {
    pub fn audio_mid(&self) -> Mid {
        self.audio
    }

    pub fn video_mid(&self) -> Mid {
        self.video
    }

    pub fn relay_only(&self) -> bool {
        self.relay_only
    }

    pub fn local_candidates(&self) -> &[String] {
        &self.local_candidates
    }

    pub fn add_remote_candidate(&mut self, candidate: &str) -> Result<(), String> {
        let cand = parse_candidate(candidate)?;
        self.rtc.add_remote_candidate(cand);
        Ok(())
    }

    /// Renegotiates the video m-line direction when camera/screen share toggles mid-call.
    pub fn set_video_send(&mut self, on: bool) -> Result<String, String> {
        if self.video == self.audio {
            return Err("no video m-line negotiated".to_owned());
        }
        if self.pending_reneg.is_some() {
            return Err("a renegotiation is already in flight".to_owned());
        }
        let dir = if on { Direction::SendRecv } else { Direction::RecvOnly };
        let mut change = self.rtc.sdp_api();
        change.set_direction(self.video, dir);
        match change.apply() {
            Some((offer, pending)) => {
                self.pending_reneg = Some(pending);
                Ok(offer.to_sdp_string())
            }
            None => Err("video direction unchanged".to_owned()),
        }
    }

    pub fn take_video_answer(&mut self, answer_sdp: &str) -> Result<(), String> {
        let Some(pending) = self.pending_reneg.take() else {
            return Err("no pending video renegotiation".to_owned());
        };
        let answer = SdpAnswer::from_sdp_string(answer_sdp).map_err(|err| format!("{err:?}"))?;
        self.rtc.sdp_api().accept_answer(pending, answer).map_err(|err| format!("{err:?}"))
    }
}

/// Runs a Signal WebRTC call loop until chatd closes stdin (hangup).
pub fn run(mut link: Link, user_id: String, peer_id: String, controls: Arc<Controls>) {
    let _audio_session = crate::platform::call_audio();
    let mut microphone_unasked = false;
    if !Audio::simulated() {
        match microphone() {
            Some(Microphone::Denied) => out::fail(CallFailure::Audio, MICROPHONE_DENIED),
            Some(Microphone::Unasked) => microphone_unasked = true,
            Some(Microphone::Allowed) | None => {}
        }
    }
    let _timer = rt::FineTimer::hold();
    rt::tick_thread();

    let quality = controls.quality();
    let (Some(mut encoder), Ok(mut poll)) = (Encoder::voice_quality(quality), Poll::new()) else {
        out::fail(CallFailure::Engine, ENGINE);
    };
    let mut encoder_audio_app = quality.prefer_audio_application();

    const UDP_TOKEN: Token = Token(1);
    if let Some(udp) = link.udp.as_mut() {
        if let Err(err) = poll.registry().register(udp, UDP_TOKEN, Interest::READABLE) {
            tracing::warn!("mediad: signal udp register failed: {err}");
            out::fail(CallFailure::Engine, ENGINE);
        }
    }

    if link.video_initial {
        controls.set_camera(true);
    }

    let mut events = Events::with_capacity(64);
    let mut recv_buf = vec![0u8; 65536];
    let mut audio: Option<Audio> = None;
    let mut devices: Option<Devices> = None;
    let mut gate = Gate::default();
    let mut processor = Processor::new();
    let mut limiter = Limiter::default();
    let mut peer_stream = Stream::new();
    let mut video = Video::new(controls.clone());
    let mut meter = Meter::own_path(if link.relay_only { CallPath::Relay } else { CallPath::Udp });
    let mut cue: Option<Playing> = None;
    let mut connected = false;
    let mut announced = false;
    let mut ticks: u64 = 0;
    let mut tick_at = Instant::now() + TICK;
    let mut levels = (0u8, 0u8);
    let mut rates = Rates::default();
    let mut estimate: Option<u64> = None;
    let mut sending_camera = false;
    let mut sending_share = false;
    let mut share_noted = false;
    let mut camera_noted = false;

    while !controls.hangup.load(Ordering::Relaxed) {
        // Drain remote ICE candidates delivered by chatd after session start.
        for cand in controls.take_signal_ice() {
            if let Err(err) = link.add_remote_candidate(&cand) {
                tracing::debug!("mediad: signal remote candidate ignored: {err}");
            }
        }

        // Poll str0m outputs (transmit packets, ICE/DTLS events, media data).
        let now = Instant::now();
        let next_timeout = loop {
            match link.rtc.poll_output() {
                Ok(Output::Timeout(at)) => break at,
                Ok(Output::Transmit(tx)) => {
                    if let Some(udp) = &link.udp {
                        meter.sent(tx.contents.len());
                        let _ = udp.send_to(&tx.contents, tx.destination);
                    }
                }
                Ok(Output::Event(ev)) => match ev {
                    Event::Connected => {
                        connected = true;
                        if !announced {
                            announced = true;
                            out::send(&MediaResponse::Connected { ssrc: 0, dave_version: 0 });
                        }
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        if connected {
                            connected = false;
                            out::send(&MediaResponse::Interrupted);
                        }
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed) => {
                        if !connected && announced {
                            connected = true;
                            out::send(&MediaResponse::Connected { ssrc: 0, dave_version: 0 });
                        }
                    }
                    Event::MediaData(data) => {
                        if data.mid == link.audio {
                            if let Some(stream) = peer_stream.as_mut() {
                                let arrived = Instant::now();
                                let deafened = controls.deafened.load(Ordering::Relaxed);
                                stream.last_packet = arrived;
                                stream.arrivals.arrived(data.time.numer() as u32, arrived);
                                stream.heard |= data.data[..] != SILENCE[..];
                                if data.data[..] == SILENCE[..] {
                                    stream.arrivals.paused();
                                }
                                if !deafened {
                                    stream.jitter.insert(frame_sequence(data.time.numer()), data.data.to_vec());
                                }
                            }
                        } else if data.mid == link.video && !data.data.is_empty() {
                            let key = data.is_keyframe();
                            let frame = Received {
                                codec: crate::video::VideoCodec::H264,
                                key,
                                contiguous: data.contiguous,
                                data: data.data,
                                user: peer_id.clone(),
                                kind: PictureKind::Camera,
                            };
                            if !video.picture_slot(0, frame) {
                                if let Some(stream) = link.rtc.direct_api().stream_rx_by_mid(link.video, None) {
                                    stream.request_keyframe(KeyframeRequestKind::Pli);
                                }
                            }
                        }
                    }
                    Event::KeyframeRequest(req) if req.mid == link.video => {
                        let _ = req.kind;
                        if sending_share {
                            video.ask_key(Track::Share);
                        } else if sending_camera {
                            video.ask_key(Track::High);
                        }
                    }
                    Event::EgressBitrateEstimate(BweKind::Twcc { estimate: est, .. }) => {
                        estimate = Some(est.as_u64());
                    }
                    Event::PeerStats(stats) => {
                        let pair = stats.selected_candidate_pair.as_ref();
                        meter.round_trip(pair.and_then(|p| p.current_round_trip_time), stats.rtt);
                        meter.path = if link.relay_only {
                            CallPath::Relay
                        } else if let Some(pair) = pair {
                            match pair.protocol {
                                Protocol::Udp => CallPath::Udp,
                                _ => CallPath::Tcp,
                            }
                        } else {
                            CallPath::Udp
                        };
                    }
                    _ => {}
                },
                Err(err) => {
                    tracing::warn!("mediad: signal str0m poll error: {err:?}");
                    out::fail(CallFailure::Lost, "The Signal call connection was lost.");
                }
            }
        };

        // Wait until next 20 ms tick, str0m timeout, or UDP packet.
        let wake_at = tick_at.min(next_timeout);
        let wait = wake_at.saturating_duration_since(now).min(TICK);
        let _ = poll.poll(&mut events, Some(wait));

        // Read incoming UDP packets and hand them to str0m.
        if let (Some(udp), Some(local)) = (&link.udp, link.local_udp) {
            loop {
                match udp.recv_from(&mut recv_buf) {
                    Ok((len, from)) => {
                        meter.received(len);
                        if let Ok(contents) = recv_buf[..len].try_into() {
                            let recv = Receive {
                                proto: Protocol::Udp,
                                source: from,
                                destination: local,
                                contents,
                            };
                            let _ = link.rtc.handle_input(Input::Receive(Instant::now(), recv));
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        let now = Instant::now();
        if now >= next_timeout {
            let _ = link.rtc.handle_input(Input::Timeout(now));
        }

        if now < tick_at {
            continue;
        }
        ticks += 1;
        tick_at = (tick_at + TICK).max(now);

        // Open audio devices once connected.
        if connected && audio.is_none() {
            let (in_choice, out_choice) = controls.take_chosen();
            match Audio::open(in_choice, out_choice) {
                Ok(opened) => {
                    announce(&opened);
                    devices = Some(Devices::start(microphone_unasked));
                    audio = Some(opened);
                }
                Err(err) => {
                    let (reason, detail) = media::failure(err);
                    out::fail(reason, detail);
                }
            }
        }

        // Camera & screen share state.
        let want_camera = controls.camera();
        let want_share = controls.share();
        if want_camera != sending_camera || want_share != sending_share {
            sending_camera = want_camera;
            sending_share = want_share;
            if !sending_share {
                share_noted = false;
            }
            if !sending_camera {
                camera_noted = false;
            }
            out::send(&MediaResponse::Sending { share: sending_share, camera: sending_camera });
        }
        let preview_camera = !want_camera && controls.camera_preview();
        let keep_camera = want_camera || preview_camera;
        let share_on = connected && sending_share;
        let camera_on = connected && sending_camera;
        if let Some(est) = estimate {
            rates = video::allocate(est, share_on, camera_on, rates);
        }
        video.want(Wants {
            share: share_on,
            camera: camera_on,
            preview_camera,
            keep_camera,
            rates,
        });
        while let Some(word) = video.next() {
            match word {
                Word::Frame { track: _, payload, rtp, at } => {
                    if connected && link.video != link.audio {
                        if let Some(writer) = link.rtc.writer(link.video) {
                            let pt = writer.payload_params().next().map(|p| p.pt());
                            if let Some(pt) = pt {
                                let _ = writer.write(pt, at, MediaTime::new(rtp, Frequency::NINETY_KHZ), payload);
                            }
                        }
                    }
                }
                Word::SelfView { share, painted } => out::send(&match painted {
                    Some(painted) => MediaResponse::SelfView {
                        path: painted.path.display().to_string(),
                        width: painted.width,
                        height: painted.height,
                        generation: painted.generation,
                        share,
                    },
                    None => MediaResponse::SelfView {
                        path: String::new(),
                        width: 0,
                        height: 0,
                        generation: 0,
                        share,
                    },
                }),
                Word::Failed { share: true, words } => {
                    if !share_noted {
                        share_noted = true;
                        controls.set_share(false);
                        out::send(&MediaResponse::Notice { detail: words });
                        out::send(&MediaResponse::Sending { share: false, camera: sending_camera });
                    }
                }
                Word::Failed { share: false, words } => {
                    if !camera_noted {
                        camera_noted = true;
                        controls.set_camera(false);
                        out::send(&MediaResponse::Notice { detail: words });
                        out::send(&MediaResponse::Sending { share: sending_share, camera: false });
                    }
                }
            }
        }

        // Audio capture & playback tick.
        if let Some(ref mut a) = audio {
            let quality = controls.quality();
            if quality.prefer_audio_application() != encoder_audio_app {
                if let Some(enc) = Encoder::voice_quality(quality) {
                    encoder = enc;
                    encoder_audio_app = quality.prefer_audio_application();
                }
            }
            encoder.set_bitrate(controls.bitrate());
            let muted = controls.muted.load(Ordering::Relaxed);
            let mut frame = [0i16; FRAME_SAMPLES];
            let (step, input_level) = listen(&controls, a, &mut processor, &mut gate, &mut frame);
            if let Some(speaking) = step.speaking {
                out::send(&MediaResponse::Speaking { user_id: user_id.clone(), speaking });
            }
            if connected && !(muted && step.send.is_none() && step.early.is_empty()) {
                let mut send_opus = |pcm: &[i16; FRAME_SAMPLES], tick: u64, voice: bool| {
                    let mut pkt = [0u8; 1275];
                    if let Some(len) = encoder.encode(pcm, &mut pkt) {
                        if let Some(writer) = link.rtc.writer(link.audio) {
                            let pt = writer.payload_params().next().map(|p| p.pt());
                            if let Some(pt) = pt {
                                let (lvl, v) = if len > 2 { (dbov(pcm), voice) } else { (-127, false) };
                                let ts = MediaTime::new(tick * FRAME as u64, Frequency::FORTY_EIGHT_KHZ);
                                let _ = writer.audio_level(lvl, v).write(pt, Instant::now(), ts, pkt[..len].to_vec());
                            }
                        }
                    }
                };
                let early = step.early.len() as u64;
                for (n, f) in step.early.iter().enumerate() {
                    send_opus(f, ticks.saturating_sub(early - n as u64), true);
                }
                match step.send {
                    Some(MicSend::Frame(f)) => send_opus(&f, ticks, true),
                    Some(MicSend::Silence) => send_opus(&[0i16; FRAME_SAMPLES], ticks, false),
                    None => {}
                }
            }

            let deafened = controls.deafened.load(Ordering::Relaxed);
            let ear: Ear = controls.ear();
            let mut mix = [0i32; FRAME_SAMPLES];
            if let Some(stream) = peer_stream.as_mut() {
                let loud = if deafened {
                    stream.drop_audio();
                    stream.heard
                } else {
                    match stream.pull(&format_args!("signal {peer_id}")) {
                        Some(pcm) => {
                            stream.mix_into(&mut mix, &pcm, ear.factor(&peer_id, false));
                            level(&pcm) >= VOICE
                        }
                        None => false,
                    }
                };
                if let Some(speaking) = stream.hear(loud) {
                    out::send(&MediaResponse::Speaking { user_id: peer_id.clone(), speaking });
                }
            }
            let output_level = mixdown(&mix, &controls, &mut cue, a, &mut processor, &mut limiter);
            if ticks.is_multiple_of(5) && controls.metering() && (input_level, output_level) != levels {
                levels = (input_level, output_level);
                out::send(&MediaResponse::Levels { input: input_level, output: output_level });
            }
            if ticks.is_multiple_of(SECOND) {
                a.log_glitches();
                meter.troubles = a.troubles();
                if connected {
                    let counts = peer_stream.as_mut().map(|s| s.counts()).into_iter();
                    let stats = meter.second(counts);
                    controls.publish_stats(stats);
                }
            }
            if let Some(ref mut dev) = devices {
                if ticks.is_multiple_of(SECOND) {
                    if let Err((reason, detail)) = dev.permission() {
                        out::fail(reason, detail);
                    }
                }
                if let Err((reason, detail)) = dev.service(a, &controls, ticks) {
                    out::fail(reason, detail);
                }
            }
        }
    }

    if let Some(ref mut a) = audio {
        finish_cue(&controls, &mut cue, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive_pair(caller: &mut Link, callee: &mut Link, now: &mut Instant, ms: u64) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, bool, bool) {
        let end = *now + Duration::from_millis(ms);
        let mut caller_connected = false;
        let mut callee_connected = false;
        let mut callee_audio = Vec::new();
        let mut callee_video = Vec::new();
        let caller_addr = caller.local_udp.unwrap();
        let callee_addr = callee.local_udp.unwrap();

        while *now < end {
            let next_a = loop {
                match caller.rtc.poll_output().unwrap() {
                    Output::Timeout(t) => break t,
                    Output::Transmit(tx) => {
                        let recv = Receive {
                            proto: tx.proto,
                            source: caller_addr,
                            destination: callee_addr,
                            contents: (&*tx.contents).try_into().unwrap(),
                        };
                        let _ = callee.rtc.handle_input(Input::Receive(*now, recv));
                    }
                    Output::Event(Event::Connected) => caller_connected = true,
                    Output::Event(_) => {}
                }
            };

            let next_b = loop {
                match callee.rtc.poll_output().unwrap() {
                    Output::Timeout(t) => break t,
                    Output::Transmit(tx) => {
                        let recv = Receive {
                            proto: tx.proto,
                            source: callee_addr,
                            destination: caller_addr,
                            contents: (&*tx.contents).try_into().unwrap(),
                        };
                        let _ = caller.rtc.handle_input(Input::Receive(*now, recv));
                    }
                    Output::Event(Event::Connected) => callee_connected = true,
                    Output::Event(Event::MediaData(data)) => {
                        if data.mid == callee.audio {
                            callee_audio.push(data.data.to_vec());
                        } else if data.mid == callee.video {
                            callee_video.push(data.data.to_vec());
                        }
                    }
                    Output::Event(_) => {}
                }
            };

            let step = next_a.min(next_b).min(end);
            *now = step.max(*now + Duration::from_millis(1));
            let _ = caller.rtc.handle_input(Input::Timeout(*now));
            let _ = callee.rtc.handle_input(Input::Timeout(*now));
        }

        (callee_audio, callee_video, caller_connected, callee_connected)
    }

    #[test]
    fn offer_answer_dtls_srtp_loopback_carries_opus_audio_and_h264_video() {
        let offer_spec = SignalOffer {
            user_id: "alice".into(),
            peer_id: "bob".into(),
            host: "127.0.0.1".into(),
            video: true,
            relay_only: false,
            relays: Vec::new(),
            muted: false,
            deafened: false,
        };
        let offer = Offer::new(&offer_spec).expect("offer builds");
        assert!(offer.sdp().contains("m=audio"), "SDP must offer audio");
        assert!(offer.sdp().contains("m=video"), "SDP must offer video");
        assert!(offer.sdp().contains("opus/48000"), "SDP must offer Opus");
        assert!(offer.sdp().contains("H264/90000"), "SDP must offer H.264");
        assert!(!offer.local_candidates().is_empty(), "direct mode gathers host candidate");

        let accept_spec = SignalAccept {
            user_id: "bob".into(),
            peer_id: "alice".into(),
            host: "127.0.0.1".into(),
            offer_sdp: offer.sdp().to_owned(),
            video: true,
            relay_only: false,
            relays: Vec::new(),
            muted: false,
            deafened: false,
        };
        let answer = Answer::from_offer(&accept_spec).expect("answer accepts offer");
        let answer_sdp = answer.sdp().to_owned();
        let mut callee = answer.into_link();
        let mut caller = offer.accept(&answer_sdp).expect("caller accepts answer");

        // Complete ICE + DTLS-SRTP handshake using a shared monotonic clock.
        let mut now = Instant::now();
        let (_, _, a_up, b_up) = drive_pair(&mut caller, &mut callee, &mut now, 2000);
        assert!(a_up && b_up, "both str0m peers must complete DTLS-SRTP handshake");

        // Send Opus audio and H.264 Annex-B keyframe over encrypted DTLS-SRTP.
        let audio_pt = caller.rtc.writer(caller.audio).unwrap().payload_params().next().unwrap().pt();
        let video_pt = caller.rtc.writer(caller.video).unwrap().payload_params().next().unwrap().pt();
        for n in 0..5u64 {
            caller
                .rtc
                .writer(caller.audio)
                .unwrap()
                .write(audio_pt, now, MediaTime::new(n * 960, Frequency::FORTY_EIGHT_KHZ), vec![0xf8, 0xff, 0xfe, 0x01])
                .unwrap();
        }
        let h264_idr = vec![
            0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1e, 0xda, 0x02, 0x80, 0x2d, 0x8b, 0x11,
            0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80,
            0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xa0, 0x55, 0x66, 0x77,
        ];
        caller
            .rtc
            .writer(caller.video)
            .unwrap()
            .write(video_pt, now, MediaTime::new(0, Frequency::NINETY_KHZ), h264_idr)
            .unwrap();

        let (rx_audio, rx_video, _, _) = drive_pair(&mut caller, &mut callee, &mut now, 1500);
        assert!(!rx_audio.is_empty(), "callee must receive DTLS-SRTP Opus audio frames");
        assert!(!rx_video.is_empty(), "callee must receive DTLS-SRTP H.264 video frames");
    }

    #[test]
    fn relay_only_suppresses_host_candidates_and_keeps_relay_path() {
        let relay_spec = SignalOffer {
            user_id: "alice".into(),
            peer_id: "stranger".into(),
            host: "127.0.0.1".into(),
            video: false,
            relay_only: true,
            relays: vec![SignalRelay {
                urls: vec!["turn:127.0.0.1:3478?transport=udp".into()],
                username: "u".into(),
                password: "p".into(),
            }],
            muted: false,
            deafened: false,
        };
        let offer = Offer::new(&relay_spec).expect("relay offer builds");
        assert!(!offer.sdp().contains(" typ host"), "relay-only SDP must never leak host IP: {}", offer.sdp());
        assert!(
            offer.local_candidates().iter().all(|c| c.contains("typ relay") && !c.contains("typ host")),
            "all local candidates must be relay: {:?}",
            offer.local_candidates()
        );
    }

    #[test]
    fn remote_ice_candidate_parsing_and_injection() {
        let spec = SignalOffer {
            user_id: "alice".into(),
            peer_id: "bob".into(),
            host: "127.0.0.1".into(),
            video: false,
            relay_only: false,
            relays: Vec::new(),
            muted: false,
            deafened: false,
        };
        let mut offer = Offer::new(&spec).unwrap();
        let line = offer.local_candidates()[0].clone();
        offer.add_remote_candidate(&line).expect("raw candidate parses");
        offer.add_remote_candidate(&format!("a={line}")).expect("a=-prefixed candidate parses");
        assert!(offer.add_remote_candidate("not a candidate").is_err());
    }

    #[test]
    fn video_toggle_renegotiates_video_direction() {
        let offer_spec = SignalOffer {
            user_id: "alice".into(),
            peer_id: "bob".into(),
            host: "127.0.0.1".into(),
            video: false,
            relay_only: false,
            relays: Vec::new(),
            muted: false,
            deafened: false,
        };
        let offer = Offer::new(&offer_spec).unwrap();
        let accept = Answer::from_offer(&SignalAccept {
            user_id: "bob".into(),
            peer_id: "alice".into(),
            host: "127.0.0.1".into(),
            offer_sdp: offer.sdp().to_owned(),
            video: false,
            relay_only: false,
            relays: Vec::new(),
            muted: false,
            deafened: false,
        })
        .unwrap();
        let ans_sdp = accept.sdp().to_owned();
        let mut callee = accept.into_link();
        let mut caller = offer.accept(&ans_sdp).unwrap();

        let reneg_sdp = caller.set_video_send(true).expect("video renegotiation offer");
        let reneg_ans = callee
            .rtc
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(&reneg_sdp).unwrap())
            .unwrap();
        caller.take_video_answer(&reneg_ans.to_sdp_string()).expect("renegotiation answer applied");
        assert_eq!(caller.rtc.media(caller.video).unwrap().direction(), Direction::SendRecv);
    }
}
