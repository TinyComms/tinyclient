//! Discord pictures on the voice UDP socket.
//!
//! A camera is up to two H.264 simulcast layers (`rid` `"100"` high, `"50"` low) on the call's own
//! voice connection, driven from the audio loop. A screen share is a second voice connection (Go
//! Live) and this module's [`run`] is its whole media loop. Each watched Go Live stream runs on its
//! own receive-only leg with its own DAVE session and can receive Opus audio as well as H.264, VP8,
//! or AV1 video. Multiple remote cameras on the voice connection decode concurrently in their own
//! decoder slots (subject to the device decode budget) and stay visible while Go Live streams are
//! watched.

use crate::dave::Dave;
use crate::media::{FromMedia, Stream};
use crate::picture::{self, camera_high_bps, camera_profile};
use crate::rtp::{self, Cipher, Sender as AudioSender, VideoSender, H264, RTX};
use crate::session::{lock, Controls};
use crate::video::{Rates, Received, Track, Video, VideoCodec, Wants, Word};
use crate::{av1, h264, hear::Ear, opus, out, rt, share_audio, vp8, FRAME, FRAME_SAMPLES};
use protocol::{MediaResponse, PictureKind, WatchSize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the gateway says when a send was asked for and Ready named no video SSRC.
pub const NO_ASSIGNED: &str = "Discord didn't assign a video stream on this call.";

/// Whose video one SSRC carries. `repair_of` is the primary SSRC when this one retransmits it.
#[derive(Clone, Debug)]
pub struct Remote {
    pub user_id: String,
    pub repair_of: Option<u32>,
    /// The simulcast layer RID announced in op 12 (e.g. `"100"` or `"50"`).
    pub rid: String,
    /// Announced quality score (e.g. `100` or `50`).
    pub quality: u32,
    /// Whether the sender reported this layer active.
    pub active: bool,
}

struct Sent {
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
}

#[derive(Default)]
struct History {
    items: VecDeque<(u16, Sent)>,
}

impl History {
    fn remember(&mut self, sequence: u16, sent: Sent) {
        if self.items.len() >= 128 {
            self.items.pop_front();
        }
        self.items.push_back((sequence, sent));
    }

    fn recall(&self, sequence: u16) -> Option<Sent> {
        self.items.iter().rev().find(|(got, _)| *got == sequence).map(|(_, sent)| Sent {
            timestamp: sent.timestamp,
            marker: sent.marker,
            payload: sent.payload.clone(),
        })
    }
}

/// The two SSRCs we send one simulcast layer on: the picture, and the retransmission of its packets.
pub struct Pipe {
    ssrc: u32,
    rid: &'static str,
    media: VideoSender,
    rtx: VideoSender,
    history: History,
}

impl Pipe {
    pub fn new(cipher: Cipher, ssrc: u32, rtx: u32) -> Pipe {
        Pipe::with_rid(cipher, ssrc, rtx, "100")
    }

    pub fn with_rid(cipher: Cipher, ssrc: u32, rtx: u32, rid: &'static str) -> Pipe {
        Pipe {
            ssrc,
            rid,
            media: VideoSender::new(cipher.clone(), ssrc),
            rtx: VideoSender::new(cipher, rtx),
            history: History::default(),
        }
    }

    fn send_sealed(&mut self, screen: bool, socket: &UdpSocket, timestamp: u32, sealed: &[u8]) {
        let extension = rtp::video_extension_rid(self.rid, screen, false);
        for piece in h264::packetize(sealed, h264::MAX_PAYLOAD) {
            let Some((datagram, sequence)) = self.media.seal(H264, piece.marker, timestamp, &piece.payload, &extension) else {
                continue;
            };
            if socket.send(&datagram).is_ok() {
                self.history.remember(sequence, Sent { timestamp, marker: piece.marker, payload: piece.payload });
            }
        }
    }

    fn resend(&mut self, screen: bool, socket: &UdpSocket, sequence: u16) {
        let Some(sent) = self.history.recall(sequence) else {
            return;
        };
        let mut payload = sequence.to_be_bytes().to_vec();
        payload.extend_from_slice(&sent.payload);
        let extension = rtp::video_extension_rid(self.rid, screen, true);
        if let Some((datagram, _)) = self.rtx.seal(RTX, sent.marker, sent.timestamp, &payload, &extension) {
            let _ = socket.send(&datagram);
        }
    }
}

/// Per-SSRC RTP frame reassembler, dynamically matching the codec signalled by RTP payload type.
enum StreamAssembler {
    H264(h264::Assembler),
    Vp8(vp8::Assembler),
    Av1(av1::Assembler),
}

impl StreamAssembler {
    fn for_codec(codec: VideoCodec) -> Self {
        match codec {
            VideoCodec::H264 => Self::H264(h264::Assembler::default()),
            VideoCodec::Vp8 => Self::Vp8(vp8::Assembler::default()),
            VideoCodec::Av1 => Self::Av1(av1::Assembler::default()),
        }
    }

    fn codec(&self) -> VideoCodec {
        match self {
            Self::H264(_) => VideoCodec::H264,
            Self::Vp8(_) => VideoCodec::Vp8,
            Self::Av1(_) => VideoCodec::Av1,
        }
    }

    fn push(&mut self, sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::H264(a) => a.push(sequence, timestamp, marker, payload),
            Self::Vp8(a) => a.push(sequence, timestamp, marker, payload),
            Self::Av1(a) => a.push(sequence, timestamp, marker, payload),
        }
    }
}

fn frame_is_key(codec: VideoCodec, frame: &[u8]) -> bool {
    match codec {
        VideoCodec::H264 => h264::is_key(frame),
        VideoCodec::Vp8 => vp8::is_key(frame),
        VideoCodec::Av1 => av1::is_key(frame),
    }
}

struct SlotState {
    slot: usize,
    ssrc: u32,
    last_seen: Instant,
}

/// One picture leg: capture, packetizing, and the decoded pictures arriving for other people.
pub struct Drive {
    controls: Arc<Controls>,
    video: Video,
    /// This leg's picture is a screen, not a camera.
    screen: bool,
    /// This leg may send. A watch connection only receives.
    can_send: bool,
    /// High layer (or screen share) send pipe (`rid` `"100"`).
    pipe: Option<Pipe>,
    /// Optional low camera simulcast send pipe (`rid` `"50"`).
    low_pipe: Option<Pipe>,
    announced: Arc<AtomicBool>,
    videos: Arc<Mutex<HashMap<u32, Remote>>>,
    assemblers: HashMap<u32, StreamAssembler>,
    /// The control as it was when we last told the gateway.
    wanted: bool,
    /// Active decoder slots keyed by remote `user_id` so every camera / stream paints at once.
    slots: HashMap<String, SlotState>,
    free_slots: Vec<usize>,
    next_slot: usize,
    /// Remote sources dropped because this leg hit its concurrent decode ceiling.
    refused: Vec<(String, PictureKind)>,
    last_prune: Instant,
    /// Last share config version announced to the gateway.
    announced_version: u64,
    user_id: String,
}

impl Drive {
    pub fn new(
        controls: Arc<Controls>,
        user_id: String,
        videos: Arc<Mutex<HashMap<u32, Remote>>>,
        announced: Arc<AtomicBool>,
        pipe: Option<Pipe>,
        screen: bool,
        can_send: bool,
    ) -> Drive {
        Drive::new_with_low(controls, user_id, videos, announced, pipe, None, screen, can_send)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_low(
        controls: Arc<Controls>,
        user_id: String,
        videos: Arc<Mutex<HashMap<u32, Remote>>>,
        announced: Arc<AtomicBool>,
        pipe: Option<Pipe>,
        low_pipe: Option<Pipe>,
        screen: bool,
        can_send: bool,
    ) -> Drive {
        Drive {
            video: Video::new(controls.clone()),
            controls,
            screen,
            can_send,
            pipe,
            low_pipe,
            announced,
            videos,
            assemblers: HashMap::new(),
            wanted: false,
            slots: HashMap::new(),
            free_slots: Vec::new(),
            next_slot: 0,
            refused: Vec::new(),
            last_prune: Instant::now(),
            announced_version: picture::share_config_version(),
            user_id,
        }
    }

    fn wants(&self) -> Wants {
        if self.screen {
            Wants {
                share: true,
                camera: false,
                preview_camera: false,
                keep_camera: false,
                rates: Rates { share: picture::share_quality().bps, high: 0, low: 0 },
                share_version: picture::share_config_version(),
            }
        } else {
            let low = if self.low_pipe.is_some() { camera_profile().low_layer().bps } else { 0 };
            Wants {
                share: false,
                camera: true,
                preview_camera: false,
                keep_camera: true,
                rates: Rates { share: 0, high: camera_high_bps(), low },
                share_version: 0,
            }
        }
    }

    fn decode_ceiling(&self) -> usize {
        if self.screen {
            1
        } else {
            let (max_large, max_small) = crate::video::default_budget();
            (max_large + max_small).max(1)
        }
    }

    fn tile_is_small(&self, user: &str, kind: PictureKind) -> bool {
        let sources = self.controls.watch_sources();
        if let Some(entry) = sources.iter().find(|s| s.user_id == user && s.kind == kind) {
            return entry.size == WatchSize::Small;
        }
        if self.screen {
            return false;
        }
        if let Some(speaker) = self.controls.speaker() {
            return speaker != user;
        }
        // First camera tile stays full size; additional concurrent camera tiles use small budget.
        !self.slots.is_empty()
    }

    /// True when the datagram was video or RTCP, so the audio loop leaves it alone.
    pub fn take(&mut self, datagram: &[u8], opener: &Cipher, dave: &Mutex<Dave>, socket: &UdpSocket) -> bool {
        if datagram.len() < 12 || datagram[0] >> 6 != 2 {
            return false;
        }
        if (200..=206).contains(&datagram[1]) {
            self.rtcp(datagram, opener, socket);
            return true;
        }
        let ssrc = u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]);
        let remote = lock(&self.videos).get(&ssrc).cloned();
        let Some(remote) = remote else {
            return datagram[1] & 0x7f != rtp::PAYLOAD_TYPE;
        };
        if remote.user_id == self.user_id {
            return true;
        }
        let Ok(packet) = opener.unseal(datagram) else {
            return true;
        };
        if let Some(primary) = remote.repair_of {
            let codec = rtp::rtx_codec(packet.payload_type).unwrap_or(VideoCodec::H264);
            if packet.payload.len() >= 2 {
                let sequence = u16::from_be_bytes([packet.payload[0], packet.payload[1]]);
                self.assemble(primary, &remote.user_id, codec, sequence, packet.timestamp, packet.marker, &packet.payload[2..], dave);
            }
            return true;
        }
        let Some(codec) = rtp::video_codec(packet.payload_type) else {
            return true;
        };
        self.assemble(packet.ssrc, &remote.user_id, codec, packet.sequence, packet.timestamp, packet.marker, &packet.payload, dave);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        &mut self,
        ssrc: u32,
        user: &str,
        codec: VideoCodec,
        sequence: u16,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
        dave: &Mutex<Dave>,
    ) {
        let assembler = self.assemblers.entry(ssrc).or_insert_with(|| StreamAssembler::for_codec(codec));
        if assembler.codec() != codec {
            *assembler = StreamAssembler::for_codec(codec);
        }
        let Some(sealed) = assembler.push(sequence, timestamp, marker, payload) else {
            return;
        };
        let Some(id) = user.parse().ok() else {
            return;
        };
        let Some(clear) = lock(&dave).decrypt_video(Some(id), &sealed) else {
            return;
        };
        self.paint(ssrc, user, codec, clear);
    }

    fn paint(&mut self, ssrc: u32, user: &str, codec: VideoCodec, frame: Vec<u8>) {
        let now = Instant::now();
        let kind = if self.screen { PictureKind::Share } else { PictureKind::Camera };
        let slot = if let Some(state) = self.slots.get_mut(user) {
            if state.ssrc != ssrc {
                state.ssrc = ssrc;
                self.video.reset_slot(state.slot);
            }
            state.last_seen = now;
            state.slot
        } else {
            if self.slots.len() >= self.decode_ceiling() {
                let entry = (user.to_owned(), kind);
                if !self.refused.contains(&entry) {
                    self.refused.push(entry);
                    out::send(&MediaResponse::WatchRefused { refused: self.refused.clone() });
                }
                return;
            }
            let small = self.tile_is_small(user, kind);
            let slot = self.free_slots.pop().unwrap_or_else(|| {
                let idx = self.next_slot;
                self.next_slot += 1;
                idx
            });
            self.video.watch_slot(slot, small);
            self.slots.insert(user.to_owned(), SlotState { slot, ssrc, last_seen: now });
            let before = self.refused.len();
            self.refused.retain(|(uid, k)| !(uid == user && *k == kind));
            if self.refused.len() != before {
                out::send(&MediaResponse::WatchRefused { refused: self.refused.clone() });
            }
            slot
        };
        self.video.picture_slot(
            slot,
            Received {
                codec,
                data: Arc::<[u8]>::from(frame.clone()),
                key: frame_is_key(codec, &frame),
                contiguous: true,
                user: user.to_owned(),
                kind,
            },
        );
    }

    /// Drops decoder slots and assemblers for remote videos that op 12 or disconnect removed.
    fn prune_vanished(&mut self) {
        if self.last_prune.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_prune = Instant::now();
        let (active_ssrcs, active_users): (HashSet<u32>, HashSet<String>) = {
            let videos = lock(&self.videos);
            let ssrcs = videos.keys().copied().collect();
            let users = videos
                .values()
                .filter(|r| r.repair_of.is_none() && r.active)
                .map(|r| r.user_id.clone())
                .collect();
            (ssrcs, users)
        };
        self.assemblers.retain(|ssrc, _| active_ssrcs.contains(ssrc));
        let kind = if self.screen { PictureKind::Share } else { PictureKind::Camera };
        let gone: Vec<String> = self
            .slots
            .iter()
            .filter(|(uid, _)| !active_users.contains(*uid))
            .map(|(uid, _)| uid.clone())
            .collect();
        for uid in gone {
            if let Some(state) = self.slots.remove(&uid) {
                self.video.clear_slot(state.slot, uid, kind);
                self.free_slots.push(state.slot);
            }
        }
        let before = self.refused.len();
        self.refused.retain(|(uid, _)| active_users.contains(uid));
        if self.refused.len() != before {
            out::send(&MediaResponse::WatchRefused { refused: self.refused.clone() });
        }
    }

    /// Sends what the capture thread has, once the gateway has announced the SSRC. A change of
    /// the control is only reported here: nothing is sent in that same turn, because the
    /// announcement has to be on the wire first.
    pub fn tick(&mut self, dave: &Mutex<Dave>, socket: &UdpSocket, report: &Sender<FromMedia>) {
        self.prune_vanished();
        if !self.can_send {
            return;
        }
        let on = if self.screen { self.controls.share() } else { self.controls.camera() };
        if on != self.wanted {
            self.wanted = on;
            self.announced_version = picture::share_config_version();
            if !on {
                self.announced.store(false, Ordering::Relaxed);
            }
            let _ = report.send(FromMedia::Picture { share: self.screen, on });
            self.video.want(if on { self.wants() } else { Wants::default() });
            return;
        }
        if !on || !self.announced.load(Ordering::Relaxed) {
            return;
        }
        if self.screen {
            let version = picture::share_config_version();
            if version != self.announced_version {
                self.announced_version = version;
                let _ = report.send(FromMedia::Picture { share: true, on: true });
            }
        }
        self.video.want(self.wants());
        while let Some(word) = self.video.next() {
            match word {
                Word::Frame { track, payload, rtp, .. } => self.send_frame(track, dave, socket, rtp as u32, &payload),
                Word::Failed { words, .. } => out::send(&MediaResponse::Notice { detail: words }),
                // The capture thread paints my own tile: my screen, from a screen leg.
                Word::SelfView { share, painted } => out::send(&match painted {
                    Some(painted) => MediaResponse::SelfView {
                        path: painted.path.display().to_string(),
                        width: painted.width,
                        height: painted.height,
                        generation: painted.generation,
                        share,
                    },
                    None => MediaResponse::SelfView { path: String::new(), width: 0, height: 0, generation: 0, share },
                }),
            }
        }
    }

    fn send_frame(&mut self, track: Track, dave: &Mutex<Dave>, socket: &UdpSocket, timestamp: u32, frame: &[u8]) {
        let pipe = match track {
            Track::Low => self.low_pipe.as_mut(),
            Track::High | Track::Share => self.pipe.as_mut(),
        };
        let Some(pipe) = pipe else {
            return;
        };
        let Some(sealed) = lock(&dave).encrypt_video(frame) else {
            return;
        };
        pipe.send_sealed(self.screen, socket, timestamp, &sealed);
    }

    /// Handles RTCP feedback: Generic NACK (PT 205, FMT 1) and PLI/FIR keyframe requests (PT 206, FMT 1 or 4).
    fn rtcp(&mut self, datagram: &[u8], opener: &Cipher, socket: &UdpSocket) {
        let pt = datagram[1];
        let fmt = datagram[0] & 0x1f;
        if pt == 206 && (fmt == 1 || fmt == 4) {
            if datagram.len() < 8 + 16 + 4 {
                return;
            }
            let end = datagram.len() - 4;
            let Ok(counter) = datagram[end..].try_into() else {
                return;
            };
            if opener.open_aad(counter, &datagram[..8], &datagram[8..end]).is_some() {
                self.video.ask_key(if self.screen { Track::Share } else { Track::High });
            }
            return;
        }
        if datagram.len() < 8 + 16 + 4 || pt != 205 || fmt != 1 {
            return;
        }
        let end = datagram.len() - 4;
        let Ok(counter) = datagram[end..].try_into() else {
            return;
        };
        let Some(plain) = opener.open_aad(counter, &datagram[..8], &datagram[8..end]) else {
            return;
        };
        if plain.len() < 4 {
            return;
        }
        let media_ssrc = u32::from_be_bytes([plain[0], plain[1], plain[2], plain[3]]);
        let mut at = 4;
        let mut missing = Vec::new();
        while at + 4 <= plain.len() {
            let pid = u16::from_be_bytes([plain[at], plain[at + 1]]);
            let blp = u16::from_be_bytes([plain[at + 2], plain[at + 3]]);
            at += 4;
            missing.push(pid);
            for bit in 0..16 {
                if blp & (1 << bit) != 0 {
                    missing.push(pid.wrapping_add(bit + 1));
                }
            }
        }
        for sequence in missing {
            self.resend(media_ssrc, socket, sequence);
        }
    }

    fn resend(&mut self, media_ssrc: u32, socket: &UdpSocket, sequence: u16) {
        if let Some(low) = self.low_pipe.as_mut().filter(|p| p.ssrc == media_ssrc) {
            low.resend(self.screen, socket, sequence);
            return;
        }
        if let Some(high) = self.pipe.as_mut() {
            high.resend(self.screen, socket, sequence);
        }
    }

    pub fn finish(&mut self) {
        let kind = if self.screen { PictureKind::Share } else { PictureKind::Camera };
        let users: Vec<String> = self.slots.drain().map(|(uid, _)| uid).collect();
        let video = std::mem::replace(&mut self.video, Video::new(self.controls.clone()));
        video.stop();
        // Clear only this leg's active pictures so stopping one watched stream leaves other
        // watched streams and cameras on screen.
        for user_id in users {
            out::send(&MediaResponse::Picture {
                path: String::new(),
                width: 0,
                height: 0,
                generation: 0,
                user_id,
                kind,
            });
        }
    }
}

/// What a Go Live connection's media thread is given. The gateway thread keeps the websocket.
pub struct Leg {
    pub socket: UdpSocket,
    pub opener: Cipher,
    pub dave: Arc<Mutex<Dave>>,
    pub controls: Arc<Controls>,
    pub report: Sender<FromMedia>,
    pub stop: Arc<AtomicBool>,
    pub videos: Arc<Mutex<HashMap<u32, Remote>>>,
    pub announced: Arc<AtomicBool>,
    pub user_id: String,
    pub pipe: Option<Pipe>,
    /// Opus on the audio SSRC, when this leg sends. A watcher has none.
    pub audio: Option<AudioSender>,
    pub watch: bool,
    pub stream_owner: Option<String>,
    pub users: Arc<Mutex<HashMap<u32, String>>>,
}

/// One watched share's audio, buffered by RTP sequence and played on the main call's clock.
pub(crate) struct WatchAudio {
    stream: Stream,
    ssrc: u32,
    gain_key: String,
}

impl WatchAudio {
    pub(crate) fn mix(&mut self, mix: &mut [i32; FRAME_SAMPLES], ear: &Ear, ducked: bool) {
        if let Some(frame) = self.stream.pull(&format_args!("Go Live audio ssrc {}", self.ssrc)) {
            self.stream.mix_into(mix, &frame, ear.factor(&self.gain_key, ducked));
        }
    }
}

/// Only the owner of this Go Live leg can contribute sound. The voice connection can have
/// the same audio SSRC, but it has a different transport cipher and DAVE session.
fn receive_audio(leg: &Leg, datagram: &[u8], owner: &str, owner_id: u64, gain_key: &str) {
    if leg.controls.deafened.load(Ordering::Relaxed) || datagram.get(1).is_none_or(|pt| pt & 0x7f != rtp::PAYLOAD_TYPE) {
        return;
    }
    let Ok(packet) = leg.opener.unseal(datagram) else {
        return;
    };
    if lock(&leg.users).get(&packet.ssrc).map(String::as_str) != Some(owner) {
        return;
    }
    let mut again = Vec::new();
    let frame = lock(&leg.dave).open(Some(owner_id), &packet.payload, &mut again);
    if !again.is_empty() {
        let _ = leg.report.send(FromMedia::Dave(again));
    }
    let Ok(frame) = frame else {
        return;
    };
    // Hold the SSRC map through insertion so a disconnect or remap can clear the old audio.
    let users = lock(&leg.users);
    if users.get(&packet.ssrc).map(String::as_str) != Some(owner) {
        return;
    }
    let mut playing = lock(&leg.controls.watched_audio);
    // Stop and deafen can clear the buffer while authentication is in progress.
    if leg.controls.leg_done(true, Some(owner)) || leg.controls.deafened.load(Ordering::Relaxed) {
        return;
    }
    let needs_new = playing.get(owner).is_none_or(|audio| audio.ssrc != packet.ssrc);
    if needs_new {
        if let Some(stream) = Stream::new() {
            playing.insert(owner.to_owned(), WatchAudio { stream, ssrc: packet.ssrc, gain_key: gain_key.to_owned() });
        }
    }
    if let Some(audio) = playing.get_mut(owner) {
        let now = Instant::now();
        audio.stream.last_packet = now;
        audio.stream.arrivals.arrived(packet.timestamp, now);
        if frame == rtp::SILENCE {
            audio.stream.arrivals.paused();
        }
        audio.stream.jitter.insert(packet.sequence, frame);
    }
}

/// Quiet frames after the sound drops before the speaking flag is cleared.
const SOUND_TRAIL: u32 = 5;
/// Below this the share is not "making sound". The 440 Hz harness sits far above it.
const SOUND_GATE: u8 = 8;
const SOUND_BITRATE: i32 = 128_000;

/// The share's sound on a sending Go Live leg. Opened only once sound is asked for, so a
/// silent share never touches a loopback device.
struct Sound {
    sender: AudioSender,
    encoder: Option<opus::Encoder>,
    capture: Option<Box<dyn share_audio::Pull>>,
    failed: bool,
    speaking: bool,
    quiet: u32,
}

impl Sound {
    fn tick(&mut self, leg: &Leg) {
        let wanted = leg.controls.share() && leg.controls.share_sound();
        if !wanted {
            self.capture = None;
            self.failed = false;
            self.quiet = 0;
            if self.speaking {
                let _ = leg.report.send(FromMedia::Soundshare(false));
                self.speaking = false;
            }
            self.sender.advance(FRAME as u32);
            return;
        }
        if self.capture.is_none() && !self.failed {
            match share_audio::open(crate::picture::chosen_screen().as_deref()) {
                Ok(pull) => self.capture = Some(pull),
                Err(words) => {
                    self.failed = true;
                    tracing::warn!("mediad: share audio: {words}");
                    out::send(&MediaResponse::Notice { detail: words });
                }
            }
        }
        let Some(frame) = self.capture.as_mut().and_then(|capture| capture.pull()) else {
            self.sender.advance(FRAME as u32);
            return;
        };
        if crate::audio::level(&frame) >= SOUND_GATE {
            self.quiet = 0;
            if !self.speaking {
                let _ = leg.report.send(FromMedia::Soundshare(true));
                self.speaking = true;
            }
            let mut packet = [0u8; 1500];
            if let Some(len) = self.encoder.as_mut().and_then(|encoder| encoder.encode(&frame, &mut packet)) {
                let sealed = lock(&leg.dave).encrypt(&packet[..len]);
                if let Some(sealed) = sealed {
                    if let Some(datagram) = self.sender.seal(&sealed) {
                        let _ = leg.socket.send(&datagram);
                    }
                }
            }
        } else if self.speaking {
            self.quiet += 1;
            if self.quiet > SOUND_TRAIL {
                let _ = leg.report.send(FromMedia::Soundshare(false));
                self.speaking = false;
                self.quiet = 0;
            }
        }
        self.sender.advance(FRAME as u32);
    }
}

/// The media loop of a stream connection. Returns when the leg is stopped. It does not end
/// the process: the voice call's thread is the one that does.
pub fn run(mut leg: Leg) {
    let _timer = rt::FineTimer::hold();
    rt::tick_thread();
    let mut sound = leg.audio.take().map(|sender| Sound {
        sender,
        encoder: opus::Encoder::at(SOUND_BITRATE),
        capture: None,
        failed: false,
        speaking: false,
        quiet: 0,
    });
    let mut drive = Drive::new(leg.controls.clone(), leg.user_id.clone(), leg.videos.clone(), leg.announced.clone(), leg.pipe.take(), true, !leg.watch);
    let owner = leg.stream_owner.as_deref().filter(|_| leg.watch).and_then(|owner| {
        Some((owner.to_owned(), owner.parse().ok()?, protocol::discord_stream_gain_key(owner)))
    });
    let mut datagram = [0u8; 2048];
    let mut next = Instant::now();
    let tick = Duration::from_millis(20);
    while !leg.stop.load(Ordering::Relaxed)
        && !out::over()
        && !leg.controls.leg_done(leg.watch, leg.stream_owner.as_deref())
    {
        let now = Instant::now();
        if now < next {
            let _ = leg.socket.set_read_timeout(Some((next - now).max(Duration::from_micros(200))));
            if let Ok(len) = leg.socket.recv(&mut datagram) {
                let datagram = &datagram[..len];
                if !drive.take(datagram, &leg.opener, &leg.dave, &leg.socket) {
                    if let Some((owner, owner_id, gain_key)) = &owner {
                        receive_audio(&leg, datagram, owner, *owner_id, gain_key);
                    }
                }
            }
            continue;
        }
        next += tick;
        if now > next + tick * 5 {
            next = now + tick;
        }
        drive.tick(&leg.dave, &leg.socket, &leg.report);
        if let Some(sound) = sound.as_mut() {
            sound.tick(&leg);
        }
    }
    if let Some((owner, _, _)) = &owner {
        lock(&leg.controls.watched_audio).remove(owner);
    }
    drive.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_share_drive_asks_for_the_bitrate_of_the_quality_that_was_picked() {
        let drive = Drive::new(
            Arc::new(Controls::default()),
            "1".into(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            None,
            true,
            true,
        );
        crate::picture::choose_quality(Some(protocol::ShareQuality::Balanced));
        assert_eq!(drive.wants().rates.share, 4_000_000);
        crate::picture::choose_quality(Some(protocol::ShareQuality::Low));
        assert_eq!(drive.wants().rates.share, 1_500_000);
        crate::picture::choose_quality(Some(protocol::ShareQuality::Best));
        assert_eq!(drive.wants().rates.share, 8_000_000);
        let cipher = Cipher::new(rtp::AES_GCM, &[7u8; 32]).unwrap();
        let camera = Drive::new_with_low(
            Arc::new(Controls::default()),
            "1".into(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Some(Pipe::with_rid(cipher.clone(), 100, 101, "100")),
            Some(Pipe::with_rid(cipher, 200, 201, "50")),
            false,
            true,
        );
        assert_eq!(camera.wants().rates.share, 0, "a camera does not spend the share's rate");
        assert!(camera.wants().rates.high > 0 && camera.wants().rates.low > 0, "simulcast camera asks for high and low layers");
        crate::picture::choose_quality(None);
    }
}
