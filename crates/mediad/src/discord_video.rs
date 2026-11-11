//! Discord pictures on the voice UDP socket.
//!
//! A camera is one H.264 layer on the call's own voice connection, driven from the audio loop.
//! A screen share is a second voice connection (Go Live) and this module's [`run`] is its whole
//! media loop: no second microphone, no speaking indication. Both seal the access unit with DAVE
//! before packetizing, and open it only after the access unit is whole again. The decode thread's
//! own sealer is transport-only on a Discord call, and it drops a frame that still ends in DAVE's
//! marker, so the marker has to be gone before the frame is handed over.

use crate::dave::Dave;
use crate::h264::{self, Assembler};
use crate::media::FromMedia;
use crate::picture::{CAMERA_HIGH_BPS, SHARE_BPS};
use crate::rtp::{self, Cipher, VideoSender, H264, RTX};
use crate::session::Controls;
use crate::video::{Rates, Received, Video, Wants, Word};
use crate::{out, rt};
use protocol::{MediaResponse, PictureKind};
use std::collections::{HashMap, VecDeque};
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
}

struct Sent {
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
}

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

/// The two SSRCs we send on: the picture, and the retransmission of its packets.
pub struct Pipe {
    media: VideoSender,
    rtx: VideoSender,
}

impl Pipe {
    pub fn new(cipher: Cipher, ssrc: u32, rtx: u32) -> Pipe {
        Pipe {
            media: VideoSender::new(cipher.clone(), ssrc),
            rtx: VideoSender::new(cipher, rtx),
        }
    }
}

/// One picture leg: capture, packetizing, and the access units arriving for other people.
pub struct Drive {
    controls: Arc<Controls>,
    video: Video,
    /// This leg's picture is a screen, not a camera.
    screen: bool,
    /// This leg may send. A watch connection only receives.
    can_send: bool,
    pipe: Option<Pipe>,
    announced: Arc<AtomicBool>,
    videos: Arc<Mutex<HashMap<u32, Remote>>>,
    assemblers: HashMap<u32, Assembler>,
    history: History,
    /// The control as it was when we last told the gateway.
    wanted: bool,
    shown: Option<String>,
    seen: HashMap<String, Instant>,
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
        Drive {
            video: Video::new(controls.clone()),
            controls,
            screen,
            can_send,
            pipe,
            announced,
            videos,
            assemblers: HashMap::new(),
            history: History { items: VecDeque::new() },
            wanted: false,
            shown: None,
            seen: HashMap::new(),
            user_id,
        }
    }

    fn wants(&self) -> Wants {
        if self.screen {
            Wants {
                share: true,
                camera: false,
                keep_camera: false,
                rates: Rates { share: SHARE_BPS, high: 0, low: 0 },
            }
        } else {
            Wants {
                share: false,
                camera: true,
                keep_camera: true,
                rates: Rates { share: 0, high: CAMERA_HIGH_BPS, low: 0 },
            }
        }
    }

    /// True when the datagram was video or RTCP, so the audio loop leaves it alone.
    pub fn take(&mut self, datagram: &[u8], opener: &Cipher, dave: &Mutex<Dave>, socket: &UdpSocket) -> bool {
        if datagram.len() < 12 || datagram[0] >> 6 != 2 {
            return false;
        }
        if (200..=206).contains(&datagram[1]) {
            self.nack(datagram, opener, socket);
            return true;
        }
        let ssrc = u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]);
        let remote = self.videos.lock().unwrap().get(&ssrc).cloned();
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
            if packet.payload.len() >= 2 {
                let sequence = u16::from_be_bytes([packet.payload[0], packet.payload[1]]);
                self.assemble(primary, &remote.user_id, sequence, packet.timestamp, packet.marker, &packet.payload[2..], dave);
            }
            return true;
        }
        self.assemble(packet.ssrc, &remote.user_id, packet.sequence, packet.timestamp, packet.marker, &packet.payload, dave);
        true
    }

    fn assemble(&mut self, ssrc: u32, user: &str, sequence: u16, timestamp: u32, marker: bool, payload: &[u8], dave: &Mutex<Dave>) {
        let assembler = self.assemblers.entry(ssrc).or_default();
        let Some(sealed) = assembler.push(sequence, timestamp, marker, payload) else {
            return;
        };
        // The watched share is the one picture on screen. A camera waits until it is gone.
        if !self.screen && self.controls.watch() {
            return;
        }
        let Some(id) = user.parse().ok() else {
            return;
        };
        let Some(clear) = dave.lock().unwrap().decrypt_video(Some(id), &sealed) else {
            return;
        };
        self.paint(user, clear);
    }

    fn paint(&mut self, user: &str, frame: Vec<u8>) {
        let now = Instant::now();
        self.seen.insert(user.to_owned(), now);
        self.seen.retain(|_, at| now.saturating_duration_since(*at) < Duration::from_secs(5));
        let speaker = self.controls.speaker();
        let chosen = if let Some(wanted) = &speaker {
            user == wanted
        } else if self.shown.as_deref() == Some(user) || self.shown.is_none() {
            true
        } else {
            self.shown.as_ref().and_then(|id| self.seen.get(id)).is_none_or(|at| now.saturating_duration_since(*at) > Duration::from_secs(1))
        };
        if !chosen {
            return;
        }
        self.shown = Some(user.to_owned());
        self.video.watch();
        let kind = if self.screen { PictureKind::Share } else { PictureKind::Camera };
        self.video.picture(Received {
            data: Arc::<[u8]>::from(frame.clone()),
            key: h264::is_key(&frame),
            contiguous: true,
            user: user.to_owned(),
            kind,
        });
    }

    /// Sends what the capture thread has, once the gateway has announced the SSRC. A change of
    /// the control is only reported here: nothing is sent in that same turn, because the
    /// announcement has to be on the wire first.
    pub fn tick(&mut self, dave: &Mutex<Dave>, socket: &UdpSocket, report: &Sender<FromMedia>) {
        if !self.can_send {
            return;
        }
        let on = if self.screen { self.controls.share() } else { self.controls.camera() };
        if on != self.wanted {
            self.wanted = on;
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
        self.video.want(self.wants());
        while let Some(word) = self.video.next() {
            match word {
                Word::Frame { payload, rtp, .. } => self.send_frame(dave, socket, rtp as u32, &payload),
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

    fn send_frame(&mut self, dave: &Mutex<Dave>, socket: &UdpSocket, timestamp: u32, frame: &[u8]) {
        let Some(sealed) = dave.lock().unwrap().encrypt_video(frame) else {
            return;
        };
        let extension = rtp::video_extension(self.screen, false);
        for piece in h264::packetize(&sealed, h264::MAX_PAYLOAD) {
            let Some(pipe) = self.pipe.as_mut() else {
                return;
            };
            let Some((datagram, sequence)) = pipe.media.seal(H264, piece.marker, timestamp, &piece.payload, &extension) else {
                continue;
            };
            if socket.send(&datagram).is_ok() {
                self.history.remember(sequence, Sent { timestamp, marker: piece.marker, payload: piece.payload });
            }
        }
    }

    /// A generic NACK (RTCP PT 205, FMT 1). The first 8 bytes are the clear header; if they are
    /// not the authenticated prefix, the packet is dropped and the call carries on.
    fn nack(&mut self, datagram: &[u8], opener: &Cipher, socket: &UdpSocket) {
        if datagram.len() < 8 + 16 + 4 || datagram[1] != 205 || datagram[0] & 0x1f != 1 {
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
            self.resend(socket, sequence);
        }
    }

    fn resend(&mut self, socket: &UdpSocket, sequence: u16) {
        let Some(sent) = self.history.recall(sequence) else {
            return;
        };
        let mut payload = sequence.to_be_bytes().to_vec();
        payload.extend_from_slice(&sent.payload);
        let Some(pipe) = self.pipe.as_mut() else {
            return;
        };
        let extension = rtp::video_extension(self.screen, true);
        if let Some((datagram, _)) = pipe.rtx.seal(RTX, sent.marker, sent.timestamp, &payload, &extension) {
            let _ = socket.send(&datagram);
        }
    }

    pub fn finish(&mut self) {
        let video = std::mem::replace(&mut self.video, Video::new(self.controls.clone()));
        video.stop();
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
    pub watch: bool,
}

/// The picture loop of a stream connection. Returns when the leg is stopped. It does not end
/// the process: the voice call's thread is the one that does.
pub fn run(leg: Leg) {
    let _timer = rt::FineTimer::hold();
    rt::tick_thread();
    let mut drive = Drive::new(leg.controls.clone(), leg.user_id.clone(), leg.videos, leg.announced, leg.pipe, true, !leg.watch);
    let mut datagram = [0u8; 2048];
    let mut next = Instant::now();
    let tick = Duration::from_millis(20);
    while !leg.stop.load(Ordering::Relaxed) && !out::over() && !leg.controls.leg_done(leg.watch) {
        let now = Instant::now();
        if now < next {
            let _ = leg.socket.set_read_timeout(Some((next - now).max(Duration::from_micros(200))));
            match leg.socket.recv(&mut datagram) {
                Ok(len) => {
                    drive.take(&datagram[..len], &leg.opener, &leg.dave, &leg.socket);
                }
                Err(_) => {}
            }
            continue;
        }
        next += tick;
        if now > next + tick * 5 {
            next = now + tick;
        }
        drive.tick(&leg.dave, &leg.socket, &leg.report);
    }
    drive.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_and_repair_packets_never_reuse_a_transport_nonce() {
        for mode in [rtp::AES_GCM, rtp::XCHACHA] {
            let key = [42; 32];
            let cipher = Cipher::new(mode, &key).unwrap();
            let mut audio = rtp::Sender::new(cipher.clone(), 9, 0, 0);
            let mut pipe = Pipe::new(cipher.clone(), 10, 11);
            let opener = Cipher::new(mode, &key).unwrap();
            let mut nonces = std::collections::HashSet::new();
            for round in 0..32 {
                // Starting a new video sender, as after a toggle, must not reset this key.
                if round % 4 == 0 {
                    pipe = Pipe::new(cipher.clone(), 10, 11);
                }
                for (packet, expected) in [
                    (audio.seal(b"audio").unwrap(), b"audio".as_slice()),
                    (pipe.media.seal(H264, true, 9000, b"frame", &[]).unwrap().0, b"frame".as_slice()),
                    (pipe.rtx.seal(RTX, true, 9000, b"repair", &[]).unwrap().0, b"repair".as_slice()),
                ] {
                    let nonce = u32::from_be_bytes(packet[packet.len() - 4..].try_into().unwrap());
                    assert!(nonces.insert(nonce), "{mode}: nonce {nonce} repeated across audio/video/RTX");
                    assert_eq!(opener.unseal(&packet).unwrap().payload, expected);
                }
            }
            assert_eq!(nonces.len(), 96);
            let mut other_transport = Pipe::new(Cipher::new(mode, &[43; 32]).unwrap(), 10, 11);
            let other = other_transport.media.seal(H264, true, 0, b"other", &[]).unwrap().0;
            assert_eq!(&other[other.len() - 4..], &[0, 0, 0, 0], "another key has its own allocator");
        }
    }
}
