//! Telegram 1:1 voice calls and group voice chats in `mediad`.
//!
//! # Architecture choice: pure-Rust `str0m` + `unsafe-libopus` vs `tgcalls`
//!
//! Telegram's official clients use `tgcalls`, a C++ wrapper around Google's `libwebrtc`
//! (requires CMake, a C++20 toolchain, and a >100 MB WebRTC checkout). `tinyclient` is built
//! without any C/C++ toolchain or system media libraries across desktop and mobile, and already
//! ships a pure-Rust WebRTC stack (`str0m`), Opus codec (`unsafe-libopus`), echo cancellation /
//! noise suppression (`sonora` + `hush-vani`), and AES/SHA-2 primitives.
//!
//! Therefore this module implements both Telegram voice modes in pure Rust:
//!
//! 1. **1:1 voice calls (`#469`, [`run_direct`])**:
//!    - Encrypts and authenticates every frame and signaling message with Telegram's MTProto 2.0
//!      call key derivation (`msg_key = SHA-256(key[88+x..88+x+32] || plaintext)[8..24]`,
//!      AES-256-IGE using `aes_key`/`aes_iv` derived from `msg_key` and the 256-byte DH key,
//!      `x = 0` for caller -> callee and `x = 8` for callee -> caller).
//!    - Sends and receives 20 ms 48 kHz Opus packets over Telegram's UDP reflector/P2P endpoints
//!      (`phoneConnection` with optional 16-byte `peer_tag` routing header) and exchanges
//!      encrypted signaling packets via `phone.sendSignalingData` / `updatePhoneCallSignalingData`
//!      (`MediaResponse::TelegramSignal` / `MediaRequest::TelegramSignal`).
//! 2. **Group voice chats (`#461`, [`GroupOffer`], [`run_group`])**:
//!    - Generates a local WebRTC transport offer via `str0m` and formats Telegram's
//!      `phone.joinGroupCall` `params` JSON (`{ufrag, pwd, fingerprints, ssrc}`).
//!    - Parses Telegram's `updateGroupCallConnection` transport JSON (`transport.{ufrag, pwd,
//!      fingerprints, candidates}` and `audio.{payload-types, rtp-hdrexts}`) into a WebRTC SDP
//!      answer for `str0m`.
//!    - Maps incoming audio SSRCs to participant user ids (`MediaRequest::TelegramGroupSources`),
//!      caps simultaneously decoded/mixed streams to the [`MAX_ACTIVE_SPEAKERS`] loudest speakers
//!      so large group voice chats stay bounded, and joins muted by default.

use crate::audio::{level, Audio};
use crate::cue::Playing;
use crate::hear::Limiter;
use crate::media::{self, announce, finish_cue, listen, mixdown, Devices, Gate, Send as GateSend, Stream};
use crate::opus::{Decoder as OpusDecoder, Encoder};
use crate::permission::{microphone, Microphone};
use crate::process::Processor;
use crate::rtp::SILENCE;
use crate::session::Controls;
use crate::stats::Meter;
use crate::video::{
    Rates, Received, Received as VideoReceived, Track, Track as VideoTrack, Video, Video as VideoEngine, VideoCodec,
    Wants, Wants as VideoWants, Word, Word as VideoWord,
};
use crate::{out, rt, share_audio, telegram_e2ee, tiny, FRAME, FRAME_SAMPLES};
use aes::Aes256;
use base64::Engine as _;
use cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use mio::net::UdpSocket as MioUdp;
use mio::{Events, Interest, Poll, Token};
use protocol::{
    CallFailure, CallPath, MediaResponse, PictureKind, RtmpStreamState, TelegramCallJoin,
    TelegramGroupOffer, TelegramGroupSource, TelegramGroupVideoSource, WatchSize, WatchSource,
};
use rand::Rng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

const TICK: Duration = Duration::from_millis(20);
const SECOND: u64 = 50;
const VOICE: u8 = 25;
const UDP_TOKEN: Token = Token(0);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum simultaneous remote streams decoded and mixed in a large group voice chat (`#461`).
pub const MAX_ACTIVE_SPEAKERS: usize = 12;

/// Number of recvonly audio tracks offered to Telegram's group voice SFU.
const GROUP_RECV_SLOTS: usize = MAX_ACTIVE_SPEAKERS;
/// Number of recvonly video slots offered for multi-picture Telegram group calls (`#467`).
const GROUP_VIDEO_RECV_SLOTS: usize = 4;
/// H.264 payload type offered to Telegram group video chats.
const TG_H264_PT: u8 = 96;
/// Constrained Baseline profile-level-id for H.264.
const TG_H264_PROFILE: u32 = 0x42e01f;

/// Magic header for encrypted 1:1 Opus frames carried over a UDP reflector/P2P link.
const DIRECT_AUDIO_MAGIC: [u8; 4] = *b"TGOP";
/// Keepalive / probe packet type.
const DIRECT_KIND_PING: u8 = 1;
/// Opus audio packet type.
const DIRECT_KIND_OPUS: u8 = 2;
/// H.264 camera video chunk packet type (`#458`).
pub const DIRECT_KIND_VIDEO_CAMERA: u8 = 3;
/// H.264 screencast video chunk packet type (`#448`).
pub const DIRECT_KIND_VIDEO_SHARE: u8 = 4;
/// Keyframe request (PLI) packet type (payload byte: 0 = camera, 1 = share).
pub const DIRECT_KIND_KEYFRAME_REQ: u8 = 5;

/// Max raw H.264 payload bytes per UDP chunk so each MTProto-sealed packet stays under MTU.
const VIDEO_CHUNK_MAX: usize = 1050;
/// Chunk sub-header inside a video packet: `[frame_id: u32 LE, frag_idx: u16 LE, frag_count: u16 LE, flags: u8]`.
const VIDEO_CHUNK_HDR: usize = 9;
const VIDEO_FLAG_KEYFRAME: u8 = 1;

/// Parsed remote peer media state from decrypted `updatePhoneCallSignalingData` JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerMediaState {
    pub camera: bool,
    pub camera_paused: bool,
    pub low_battery: bool,
    pub sharing: bool,
}

/// Builds the Telegram 1:1 in-call signaling JSON for camera & screencast state.
pub fn build_media_state_signal(camera: bool, camera_paused: bool, sharing: bool, low_battery: bool) -> Value {
    let video_num = if !camera { 0 } else if camera_paused { 1 } else { 2 };
    let video_str = if !camera { "inactive" } else if camera_paused { "paused" } else { "active" };
    let share_num = if sharing { 2 } else { 0 };
    let share_str = if sharing { "active" } else { "inactive" };
    json!({
        "@type": "mediaState",
        "type": "media-state",
        "codecs": ["H264"],
        "tracks": ["camera", "screencast"],
        "videoState": video_num,
        "camera": video_str,
        "screencastState": share_num,
        "screencast": share_str,
        "isBatteryLow": low_battery,
        "low_battery": low_battery,
    })
}

/// Parses remote camera (`active` / `inactive` / `paused`), `low_battery`, and `screencast` state
/// from either tgcalls-style `mediaState` or `media-state` JSON payloads.
pub fn parse_peer_signal(val: &Value) -> Option<PeerMediaState> {
    let kind = val
        .get("@type")
        .or_else(|| val.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let has_media_fields = val.get("videoState").is_some()
        || val.get("camera").is_some()
        || val.get("screencastState").is_some()
        || val.get("screencast").is_some()
        || val.get("isBatteryLow").is_some()
        || val.get("low_battery").is_some();
    if !matches!(kind, "mediaState" | "media-state" | "video-state") && !has_media_fields {
        return None;
    }
    let parse_track = |num_key: &str, str_key: &str| -> (bool, bool) {
        if let Some(s) = val.get(str_key).and_then(Value::as_str) {
            return match s {
                "active" => (true, false),
                "paused" => (true, true),
                _ => (false, false),
            };
        }
        if let Some(n) = val.get(num_key).and_then(Value::as_i64) {
            return match n {
                2 => (true, false),
                1 => (true, true),
                _ => (false, false),
            };
        }
        if let Some(b) = val.get(str_key).and_then(Value::as_bool) {
            return (b, false);
        }
        (false, false)
    };
    let (camera, camera_paused) = parse_track("videoState", "camera");
    let (sharing, _) = parse_track("screencastState", "screencast");
    let low_battery = val
        .get("isBatteryLow")
        .or_else(|| val.get("low_battery"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(PeerMediaState { camera, camera_paused, low_battery, sharing })
}

/// Mixes a 20 ms stereo system/window loopback frame (`share_audio`) into an outgoing 20 ms
/// microphone frame with saturating clamping so peers hear shared screen sound alongside voice.
pub fn mix_share_audio(mic: &mut [i16; FRAME_SAMPLES], share: &[i16; FRAME_SAMPLES]) {
    for (dst, &src) in mic.iter_mut().zip(share.iter()) {
        let sum = i32::from(*dst) + i32::from(src);
        *dst = sum.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    }
}

/// Splits one encoded H.264 access unit into MTU-safe MTProto-encrypted direct UDP packets.
pub fn pack_video_chunks(
    key: &[u8; 256],
    outgoing: bool,
    peer_tag: Option<&[u8]>,
    kind: u8,
    frame_id: u32,
    rtp: u32,
    keyframe: bool,
    h264: &[u8],
) -> Vec<Vec<u8>> {
    if h264.is_empty() {
        return Vec::new();
    }
    let chunks: Vec<&[u8]> = h264.chunks(VIDEO_CHUNK_MAX).collect();
    let count = chunks.len().min(u16::MAX as usize) as u16;
    let flags = if keyframe { VIDEO_FLAG_KEYFRAME } else { 0 };
    chunks
        .into_iter()
        .take(count as usize)
        .enumerate()
        .map(|(idx, part)| {
            let mut payload = Vec::with_capacity(VIDEO_CHUNK_HDR + part.len());
            payload.extend_from_slice(&frame_id.to_le_bytes());
            payload.extend_from_slice(&(idx as u16).to_le_bytes());
            payload.extend_from_slice(&count.to_le_bytes());
            payload.push(flags);
            payload.extend_from_slice(part);
            pack_direct_frame(key, outgoing, peer_tag, kind, frame_id, rtp, &payload)
        })
        .collect()
}

/// Reassembles fragmented H.264 access units arriving over Telegram 1:1 UDP for one video track.
#[derive(Default)]
pub struct VideoReassembler {
    frame_id: u32,
    expected: u16,
    received: u16,
    keyframe: bool,
    parts: Vec<Option<Vec<u8>>>,
}

impl VideoReassembler {
    pub fn push(&mut self, payload: &[u8]) -> Option<(Vec<u8>, bool)> {
        if payload.len() <= VIDEO_CHUNK_HDR {
            return None;
        }
        let frame_id = u32::from_le_bytes(payload[0..4].try_into().ok()?);
        let idx = u16::from_le_bytes(payload[4..6].try_into().ok()?) as usize;
        let count = u16::from_le_bytes(payload[6..8].try_into().ok()?);
        let flags = payload[8];
        if count == 0 || idx >= count as usize || count > 256 {
            return None;
        }
        if frame_id != self.frame_id || count != self.expected {
            self.frame_id = frame_id;
            self.expected = count;
            self.received = 0;
            self.keyframe = (flags & VIDEO_FLAG_KEYFRAME) != 0;
            self.parts = vec![None; count as usize];
        }
        if self.parts[idx].is_none() {
            self.parts[idx] = Some(payload[VIDEO_CHUNK_HDR..].to_vec());
            self.received += 1;
        }
        if self.received == self.expected {
            let mut out = Vec::new();
            for part in self.parts.drain(..) {
                out.extend_from_slice(&part?);
            }
            let key = self.keyframe;
            self.expected = 0;
            self.received = 0;
            return Some((out, key));
        }
        None
    }
}

fn is_h264_keyframe(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 < data.len() {
        let start = if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            Some(i + 3)
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            Some(i + 4)
        } else {
            None
        };
        if let Some(nal_pos) = start {
            if nal_pos < data.len() {
                let nal_type = data[nal_pos] & 0x1f;
                if nal_type == 5 || nal_type == 7 {
                    return true;
                }
            }
            i = nal_pos;
        } else {
            i += 1;
        }
    }
    false
}

const ENGINE: &str = "The call engine could not start. Try again.";
const NO_AUDIO_PATH: &str = "Couldn't reach Telegram's voice path. A VPN or firewall may be blocking it.";

/// Derives MTProto 2.0 `(aes_key, aes_iv)` from a 256-byte DH shared key, 16-byte `msg_key`,
/// and direction offset `x` (`0` for caller -> callee, `8` for callee -> caller).
pub fn mtproto_kdf(key: &[u8; 256], msg_key: &[u8; 16], x: usize) -> ([u8; 32], [u8; 32]) {
    let sha256_a: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(msg_key);
        h.update(&key[x..x + 36]);
        h.finalize().into()
    };
    let sha256_b: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(&key[40 + x..40 + x + 36]);
        h.update(msg_key);
        h.finalize().into()
    };
    let mut aes_key = [0u8; 32];
    aes_key[0..8].copy_from_slice(&sha256_a[0..8]);
    aes_key[8..24].copy_from_slice(&sha256_b[8..24]);
    aes_key[24..32].copy_from_slice(&sha256_a[24..32]);

    let mut aes_iv = [0u8; 32];
    aes_iv[0..8].copy_from_slice(&sha256_b[0..8]);
    aes_iv[8..24].copy_from_slice(&sha256_a[8..24]);
    aes_iv[24..32].copy_from_slice(&sha256_b[24..32]);

    (aes_key, aes_iv)
}

/// AES-256-IGE encryption (in-place over a 16-byte aligned buffer).
fn aes_ige_encrypt(key: &[u8; 32], iv: &[u8; 32], data: &mut [u8]) {
    debug_assert_eq!(data.len() % 16, 0);
    let cipher = Aes256::new_from_slice(key).expect("256-bit AES key");
    let mut iv1 = [0u8; 16];
    let mut iv2 = [0u8; 16];
    iv1.copy_from_slice(&iv[0..16]);
    iv2.copy_from_slice(&iv[16..32]);

    for chunk in data.chunks_exact_mut(16) {
        let mut plain = [0u8; 16];
        plain.copy_from_slice(chunk);
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = plain[i] ^ iv1[i];
        }
        let mut ga = cipher::generic_array::GenericArray::from(block);
        cipher.encrypt_block(&mut ga);
        for i in 0..16 {
            chunk[i] = ga[i] ^ iv2[i];
        }
        iv1.copy_from_slice(chunk);
        iv2 = plain;
    }
}

/// AES-256-IGE decryption (in-place over a 16-byte aligned buffer).
fn aes_ige_decrypt(key: &[u8; 32], iv: &[u8; 32], data: &mut [u8]) {
    debug_assert_eq!(data.len() % 16, 0);
    let cipher = Aes256::new_from_slice(key).expect("256-bit AES key");
    let mut iv1 = [0u8; 16];
    let mut iv2 = [0u8; 16];
    iv1.copy_from_slice(&iv[0..16]);
    iv2.copy_from_slice(&iv[16..32]);

    for chunk in data.chunks_exact_mut(16) {
        let mut ct = [0u8; 16];
        ct.copy_from_slice(chunk);
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = ct[i] ^ iv2[i];
        }
        let mut ga = cipher::generic_array::GenericArray::from(block);
        cipher.decrypt_block(&mut ga);
        for i in 0..16 {
            chunk[i] = ga[i] ^ iv1[i];
        }
        iv1 = ct;
        iv2.copy_from_slice(chunk);
    }
}

/// Seals a plaintext payload with MTProto 2.0 call encryption (`msg_key || AES-256-IGE(len || payload || pad)`).
/// `outgoing` selects `x = 0` (caller -> callee) or `x = 8` (callee -> caller).
pub fn seal_mtproto(key: &[u8; 256], outgoing: bool, plaintext: &[u8]) -> Vec<u8> {
    let x = if outgoing { 0 } else { 8 };
    let mut padded = Vec::with_capacity(4 + plaintext.len() + 32);
    padded.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    padded.extend_from_slice(plaintext);
    // At least 12 bytes of random padding, aligned to 16 bytes (MTProto 2.0 rule).
    let rem = (padded.len() + 12) % 16;
    let pad_len = 12 + if rem == 0 { 0 } else { 16 - rem };
    let mut rng = rand::thread_rng();
    for _ in 0..pad_len {
        padded.push(rng.gen());
    }

    let msg_key_large: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(&key[88 + x..88 + x + 32]);
        h.update(&padded);
        h.finalize().into()
    };
    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&msg_key_large[8..24]);

    let (aes_key, aes_iv) = mtproto_kdf(key, &msg_key, x);
    aes_ige_encrypt(&aes_key, &aes_iv, &mut padded);

    let mut out = Vec::with_capacity(16 + padded.len());
    out.extend_from_slice(&msg_key);
    out.extend_from_slice(&padded);
    out
}

/// Opens and authenticates an MTProto 2.0 call packet (`msg_key || ciphertext`).
/// `from_caller` selects `x = 0` when decrypting a packet sent by the caller, or `x = 8` from callee.
pub fn open_mtproto(key: &[u8; 256], from_caller: bool, packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < 32 || (packet.len() - 16) % 16 != 0 {
        return None;
    }
    let x = if from_caller { 0 } else { 8 };
    let mut msg_key = [0u8; 16];
    msg_key.copy_from_slice(&packet[0..16]);
    let mut plain = packet[16..].to_vec();
    let (aes_key, aes_iv) = mtproto_kdf(key, &msg_key, x);
    aes_ige_decrypt(&aes_key, &aes_iv, &mut plain);

    let expected_large: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(&key[88 + x..88 + x + 32]);
        h.update(&plain);
        h.finalize().into()
    };
    // Constant-time compare of the 16-byte msg_key.
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= msg_key[i] ^ expected_large[8 + i];
    }
    if diff != 0 {
        return None;
    }
    if plain.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]) as usize;
    if 4 + len > plain.len() || plain.len() - (4 + len) < 12 {
        return None;
    }
    Some(plain[4..4 + len].to_vec())
}

/// Encodes one 1:1 direct UDP frame (optional 16-byte reflector `peer_tag` + MTProto sealed payload).
pub fn pack_direct_frame(
    key: &[u8; 256],
    outgoing: bool,
    peer_tag: Option<&[u8]>,
    kind: u8,
    seq: u32,
    timestamp: u32,
    opus: &[u8],
) -> Vec<u8> {
    let mut inner = Vec::with_capacity(13 + opus.len());
    inner.extend_from_slice(&DIRECT_AUDIO_MAGIC);
    inner.push(kind);
    inner.extend_from_slice(&seq.to_le_bytes());
    inner.extend_from_slice(&timestamp.to_le_bytes());
    inner.extend_from_slice(opus);

    let sealed = seal_mtproto(key, outgoing, &inner);
    if let Some(tag) = peer_tag.filter(|t| t.len() == 16) {
        let mut out = Vec::with_capacity(16 + sealed.len());
        out.extend_from_slice(tag);
        out.extend_from_slice(&sealed);
        out
    } else {
        sealed
    }
}

/// Decodes one 1:1 direct UDP frame, returning `(kind, seq, timestamp, payload)`.
pub fn unpack_direct_frame(
    key: &[u8; 256],
    expect_from_caller: bool,
    peer_tag: Option<&[u8]>,
    raw: &[u8],
) -> Option<(u8, u32, u32, Vec<u8>)> {
    let body = match peer_tag.filter(|t| t.len() == 16) {
        Some(tag) if raw.len() >= 16 && &raw[..16] == tag => &raw[16..],
        Some(_) if raw.len() >= 48 => raw,
        _ => raw,
    };
    let inner = open_mtproto(key, expect_from_caller, body)?;
    if inner.len() < 13 || inner[0..4] != DIRECT_AUDIO_MAGIC {
        return None;
    }
    let kind = inner[4];
    let seq = u32::from_le_bytes([inner[5], inner[6], inner[7], inner[8]]);
    let ts = u32::from_le_bytes([inner[9], inner[10], inner[11], inner[12]]);
    Some((kind, seq, ts, inner[13..].to_vec()))
}

fn resolve_direct_endpoint(join: &TelegramCallJoin) -> Option<(SocketAddr, Option<Vec<u8>>)> {
    // Prefer plain UDP reflector / P2P endpoints first, then any IPv4 endpoint.
    let pick = join
        .connections
        .iter()
        .find(|e| !e.webrtc && e.ip.parse::<IpAddr>().is_ok() && e.port > 0)
        .or_else(|| join.connections.iter().find(|e| e.ip.parse::<IpAddr>().is_ok() && e.port > 0))?;
    let ip: IpAddr = pick.ip.parse().ok()?;
    let tag = (!pick.peer_tag.is_empty())
        .then(|| base64::engine::general_purpose::STANDARD.decode(&pick.peer_tag).ok())
        .flatten()
        .filter(|b| !b.is_empty());
    Some((SocketAddr::new(ip, pick.port), tag))
}

fn decode_dh_key(key_b64: &str) -> Option<[u8; 256]> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(key_b64).ok()?;
    bytes.try_into().ok()
}

/// Runs a 1:1 Telegram voice call (`#469`).
pub fn run_direct(join: TelegramCallJoin, controls: Arc<Controls>) {
    rt::tick_thread();
    let _timer = rt::FineTimer::hold();

    let Some(key) = decode_dh_key(&join.key) else {
        out::fail(CallFailure::Encryption, "Invalid Telegram call encryption key.");
    };
    let Some(mut encoder) = Encoder::voice() else {
        out::fail(CallFailure::Engine, ENGINE);
    };
    let Some(mut remote_stream) = Stream::clocked() else {
        out::fail(CallFailure::Engine, ENGINE);
    };

    let socket = match UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))) {
        Ok(sock) => {
            let _ = sock.set_nonblocking(true);
            sock
        }
        Err(err) => {
            tracing::warn!("mediad: telegram direct udp bind failed: {err}");
            out::fail(CallFailure::Lost, NO_AUDIO_PATH);
        }
    };

    let target = resolve_direct_endpoint(&join);
    let peer_tag = target.as_ref().and_then(|(_, tag)| tag.clone());
    let mut remote_addr = target.as_ref().map(|(addr, _)| *addr);
    let local_addr = socket.local_addr().ok();

    // Send an encrypted signaling hello over Telegram's signaling channel (`phone.sendSignalingData`)
    // so both sides verify the MTProto call key over signaling and negotiate H.264 camera + screencast tracks.
    let hello = json!({
        "type": "tinyclient-call-v2",
        "role": if join.outgoing { "caller" } else { "callee" },
        "port": local_addr.map(|a| a.port()).unwrap_or(0),
        "video": join.video,
        "codecs": ["H264"],
        "tracks": ["camera", "screencast"],
    });
    let sealed_hello = seal_mtproto(&key, join.outgoing, hello.to_string().as_bytes());
    out::send(&MediaResponse::TelegramSignal {
        data: base64::engine::general_purpose::STANDARD.encode(&sealed_hello),
    });

    // Send initial encrypted UDP probe if we have a reflector/peer endpoint.
    if let Some(addr) = remote_addr {
        let ping = pack_direct_frame(&key, join.outgoing, peer_tag.as_deref(), DIRECT_KIND_PING, 0, 0, &[]);
        let _ = socket.send_to(&ping, addr);
    }

    let microphone_unasked = matches!(microphone(), Some(Microphone::Unasked));
    let (input, output) = controls.take_chosen();
    let mut audio = match Audio::open(input, output) {
        Ok(audio) => {
            announce(&audio);
            audio
        }
        Err(err) => {
            let (reason, detail) = media::failure(err);
            out::fail(reason, detail);
        }
    };
    let mut devices = Devices::start(microphone_unasked);

    out::send(&MediaResponse::Connected { ssrc: 0, dave_version: 0 });

    let mut gate = Gate::default();
    let mut processor = Processor::new();
    let mut limiter = Limiter::default();
    let mut meter = Meter::new(CallPath::Udp);
    let mut cue: Option<Playing> = None;
    let mut video = Video::new(controls.clone());
    let mut sending_camera = false;
    let mut sending_share = false;
    let mut share_noted = false;
    let mut camera_noted = false;
    let mut share_audio_cap: Option<Box<dyn share_audio::Pull>> = None;
    let mut share_audio_wanted = false;
    let mut cam_reasm = VideoReassembler::default();
    let mut share_reasm = VideoReassembler::default();
    let mut video_frame_id = 1u32;
    let rates = Rates::default();
    let mut seq = 1u32;
    let mut ticks = 0u64;
    let mut levels = (0u8, 0u8);
    let mut recv_buf = [0u8; 2048];
    let mut opus_buf = [0u8; 1275];
    let mut tick_at = Instant::now() + TICK;
    let expect_from_caller = !join.outgoing;

    while !controls.hangup.load(Ordering::Relaxed) && !out::over() {
        // Drain incoming encrypted signaling messages (`updatePhoneCallSignalingData`).
        for sig in controls.take_tg_signals() {
            if let Some(plain) = open_mtproto(&key, expect_from_caller, &sig) {
                if let Ok(val) = serde_json::from_slice::<Value>(&plain) {
                    tracing::debug!(kind = ?val.get("type"), "mediad: telegram signaling received");
                    if let Some(peer_state) = parse_peer_signal(&val) {
                        if !peer_state.camera {
                            video.clear_slot(0, join.peer_id.clone(), PictureKind::Camera);
                        }
                        if !peer_state.sharing {
                            video.clear_slot(1, join.peer_id.clone(), PictureKind::Share);
                        }
                        out::send(&MediaResponse::TelegramPeerState {
                            user_id: join.peer_id.clone(),
                            camera: peer_state.camera,
                            camera_paused: peer_state.camera_paused,
                            low_battery: peer_state.low_battery,
                            sharing: peer_state.sharing,
                        });
                    }
                }
            }
        }

        // Drain incoming encrypted UDP packets (audio + H.264 camera + H.264 screencast + PLI).
        loop {
            match socket.recv_from(&mut recv_buf) {
                Ok((n, src)) => {
                    meter.received(n);
                    if let Some((kind, pkt_seq, ts, payload)) =
                        unpack_direct_frame(&key, expect_from_caller, peer_tag.as_deref(), &recv_buf[..n])
                    {
                        if remote_addr.is_none() {
                            remote_addr = Some(src);
                        }
                        match kind {
                            DIRECT_KIND_OPUS if !payload.is_empty() => {
                                let now = Instant::now();
                                let deafened = controls.deafened.load(Ordering::Relaxed);
                                remote_stream.last_packet = now;
                                remote_stream.arrivals.arrived(ts, now);
                                remote_stream.heard |= payload[..] != SILENCE[..];
                                if !deafened {
                                    remote_stream.took(Duration::ZERO);
                                    remote_stream.jitter.insert(pkt_seq as u16, payload);
                                }
                            }
                            DIRECT_KIND_VIDEO_CAMERA => {
                                if let Some((h264, is_key)) = cam_reasm.push(&payload) {
                                    let frame = Received {
                                        codec: VideoCodec::H264,
                                        key: is_key,
                                        contiguous: true,
                                        data: Arc::from(h264),
                                        user: join.peer_id.clone(),
                                        kind: PictureKind::Camera,
                                    };
                                    if !video.picture_slot(0, frame) {
                                        if let Some(addr) = remote_addr {
                                            let pli = pack_direct_frame(&key, join.outgoing, peer_tag.as_deref(), DIRECT_KIND_KEYFRAME_REQ, seq, 0, &[0]);
                                            seq = seq.wrapping_add(1);
                                            let _ = socket.send_to(&pli, addr);
                                        }
                                    }
                                }
                            }
                            DIRECT_KIND_VIDEO_SHARE => {
                                if let Some((h264, is_key)) = share_reasm.push(&payload) {
                                    let frame = Received {
                                        codec: VideoCodec::H264,
                                        key: is_key,
                                        contiguous: true,
                                        data: Arc::from(h264),
                                        user: join.peer_id.clone(),
                                        kind: PictureKind::Share,
                                    };
                                    if !video.picture_slot(1, frame) {
                                        if let Some(addr) = remote_addr {
                                            let pli = pack_direct_frame(&key, join.outgoing, peer_tag.as_deref(), DIRECT_KIND_KEYFRAME_REQ, seq, 0, &[1]);
                                            seq = seq.wrapping_add(1);
                                            let _ = socket.send_to(&pli, addr);
                                        }
                                    }
                                }
                            }
                            DIRECT_KIND_KEYFRAME_REQ => {
                                match payload.first().copied() {
                                    Some(1) => video.ask_key(Track::Share),
                                    _ => video.ask_key(Track::High),
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        let now = Instant::now();
        if now < tick_at {
            std::thread::sleep((tick_at - now).min(Duration::from_millis(5)));
            continue;
        }
        tick_at = next_tick(tick_at, now);
        ticks += 1;

        if ticks % SECOND == 0 {
            if let Err((reason, detail)) = devices.permission() {
                out::fail(reason, detail);
            }
        }
        if let Err((reason, detail)) = devices.service(&mut audio, &controls, ticks) {
            out::fail(reason, detail);
        }

        // Camera & screencast send state + in-call signaling announcement.
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
            let state_sig = build_media_state_signal(sending_camera, false, sending_share, false);
            let sealed = seal_mtproto(&key, join.outgoing, state_sig.to_string().as_bytes());
            out::send(&MediaResponse::TelegramSignal {
                data: base64::engine::general_purpose::STANDARD.encode(&sealed),
            });
        }

        // Shared screen/window loopback audio capture (`#448`, WASAPI / ScreenCaptureKit / PipeWire + echo subtraction).
        let want_share_sound = sending_share && controls.share_sound();
        if want_share_sound != share_audio_wanted {
            share_audio_wanted = want_share_sound;
            if want_share_sound {
                match share_audio::open(crate::picture::chosen_screen().as_deref()) {
                    Ok(cap) => share_audio_cap = Some(cap),
                    Err(detail) => {
                        tracing::warn!("mediad: telegram share audio unavailable: {detail}");
                        share_audio_cap = None;
                        out::send(&MediaResponse::Notice { detail });
                    }
                }
            } else {
                share_audio_cap = None;
            }
        }

        let preview_camera = !want_camera && controls.camera_preview();
        let keep_camera = want_camera || preview_camera;
        video.want(Wants {
            share: sending_share,
            camera: sending_camera,
            preview_camera,
            keep_camera,
            rates,
            share_version: crate::picture::share_config_version(),
        });
        while let Some(word) = video.next() {
            match word {
                Word::Frame { track, payload, rtp, .. } => {
                    if let Some(addr) = remote_addr {
                        let kind = match track {
                            Track::Share => DIRECT_KIND_VIDEO_SHARE,
                            Track::High | Track::Low => DIRECT_KIND_VIDEO_CAMERA,
                        };
                        let keyframe = is_h264_keyframe(&payload);
                        let pkts = pack_video_chunks(
                            &key,
                            join.outgoing,
                            peer_tag.as_deref(),
                            kind,
                            video_frame_id,
                            rtp as u32,
                            keyframe,
                            &payload,
                        );
                        video_frame_id = video_frame_id.wrapping_add(1);
                        for pkt in pkts {
                            if socket.send_to(&pkt, addr).is_ok() {
                                meter.sent(pkt.len());
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
                    None => MediaResponse::SelfView { path: String::new(), width: 0, height: 0, generation: 0, share },
                }),
                Word::Failed { share: true, words } => {
                    if !share_noted {
                        share_noted = true;
                        controls.set_share(false);
                        controls.set_share_sound(false);
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

        encoder.set_bitrate(controls.bitrate());

        // Capture microphone + optional share_audio loopback, mix, encode Opus, encrypt with MTProto 2.0, and transmit.
        let mut frame = [0i16; FRAME_SAMPLES];
        let (step, input_level) = listen(&controls, &mut audio, &mut processor, &mut gate, &mut frame);
        if let Some(speaking) = step.speaking {
            out::send(&MediaResponse::Speaking { user_id: join.user_id.clone(), speaking });
        }
        let share_pcm = share_audio_cap.as_mut().and_then(|cap| cap.pull());
        if let Some(addr) = remote_addr {
            for early in step.early {
                if let Some(len) = encoder.encode(&early, &mut opus_buf) {
                    let pkt = pack_direct_frame(
                        &key,
                        join.outgoing,
                        peer_tag.as_deref(),
                        DIRECT_KIND_OPUS,
                        seq,
                        seq.wrapping_mul(FRAME as u32),
                        &opus_buf[..len],
                    );
                    seq = seq.wrapping_add(1);
                    if socket.send_to(&pkt, addr).is_ok() {
                        meter.sent(pkt.len());
                    }
                }
            }
            let mixed_frame = match (step.send, share_pcm) {
                (Some(GateSend::Frame(mut f)), Some(s)) => {
                    mix_share_audio(&mut f, &s);
                    Some(GateSend::Frame(f))
                }
                (None, Some(s)) | (Some(GateSend::Silence), Some(s)) => Some(GateSend::Frame(s)),
                (send, None) => send,
            };
            if let Some(send) = mixed_frame {
                let encoded: Option<&[u8]> = match send {
                    GateSend::Frame(f) => encoder.encode(&f, &mut opus_buf).map(|len| &opus_buf[..len]),
                    GateSend::Silence => Some(&SILENCE),
                };
                if let Some(payload) = encoded {
                    let pkt = pack_direct_frame(
                        &key,
                        join.outgoing,
                        peer_tag.as_deref(),
                        DIRECT_KIND_OPUS,
                        seq,
                        seq.wrapping_mul(FRAME as u32),
                        payload,
                    );
                    seq = seq.wrapping_add(1);
                    if socket.send_to(&pkt, addr).is_ok() {
                        meter.sent(pkt.len());
                    }
                }
            } else if ticks % (SECOND * 2) == 0 {
                let ping = pack_direct_frame(
                    &key,
                    join.outgoing,
                    peer_tag.as_deref(),
                    DIRECT_KIND_PING,
                    seq,
                    seq.wrapping_mul(FRAME as u32),
                    &[],
                );
                let _ = socket.send_to(&ping, addr);
            }
        }

        // Playout from the remote party.
        let deafened = controls.deafened.load(Ordering::Relaxed);
        let ear = controls.ear();
        let mut mix = [0i32; FRAME_SAMPLES];
        let loud = if deafened {
            remote_stream.drop_audio();
            remote_stream.heard
        } else if let Some(pcm) = remote_stream.pull(&join.peer_id) {
            remote_stream.mix_into(&mut mix, &pcm, ear.factor(&join.peer_id, false));
            level(&pcm) >= VOICE
        } else {
            false
        };
        if let Some(speaking) = remote_stream.hear(loud) {
            out::send(&MediaResponse::Speaking { user_id: join.peer_id.clone(), speaking });
        }
        let output_level = mixdown(&mix, &controls, &mut cue, &mut audio, &mut processor, &mut limiter);
        levels = (levels.0.max(input_level), levels.1.max(output_level));

        if ticks % 5 == 0 && controls.metering() {
            out::send(&MediaResponse::Levels { input: levels.0, output: levels.1 });
            levels = (0, 0);
        }
        if ticks % SECOND == 0 {
            let stats = meter.second(std::iter::once(remote_stream.counts()));
            controls.publish_stats(stats);
        }
    }

    finish_cue(&controls, &mut cue, &mut audio);
}

// ---------------------------------------------------------------------------
// Group Voice Chats (`#461`)
// ---------------------------------------------------------------------------

fn group_rtc_config() -> RtcConfig {
    let mut config = RtcConfig::new()
        .clear_codecs()
        .enable_opus(true, false)
        .set_stats_interval(Some(Duration::from_secs(1)))
        .set_reordering_size_audio(0)
        .set_reordering_timeout_audio(Some(Duration::from_millis(40)));
    config
        .codec_config()
        .add_h264(TG_H264_PT.into(), None, true, TG_H264_PROFILE);
    config.set_max_stun_rto(Duration::from_secs(1));
    config.set_max_stun_retransmits(7);
    match tiny::dtls_cert() {
        Some(cert) => config.set_dtls_cert(cert),
        None => config,
    }
}

fn local_ip() -> IpAddr {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok();
    probe
        .and_then(|sock| {
            sock.connect(SocketAddr::from(([192, 0, 2, 1], 9))).ok()?;
            let ip = sock.local_addr().ok()?.ip();
            (!ip.is_unspecified()).then_some(ip)
        })
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// Extracts `(ufrag, pwd, sha256_fingerprint)` from a str0m-generated SDP offer string.
fn parse_offer_ice_dtls(sdp: &str) -> Result<(String, String, String), String> {
    let (mut ufrag, mut pwd, mut fp) = (None, None, None);
    for line in sdp.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("a=ice-ufrag:") {
            ufrag.get_or_insert_with(|| v.to_owned());
        } else if let Some(v) = line.strip_prefix("a=ice-pwd:") {
            pwd.get_or_insert_with(|| v.to_owned());
        } else if let Some(v) = line.strip_prefix("a=fingerprint:sha-256 ") {
            fp.get_or_insert_with(|| v.to_owned());
        }
    }
    Ok((
        ufrag.ok_or_else(|| "missing ice-ufrag in local SDP".to_owned())?,
        pwd.ok_or_else(|| "missing ice-pwd in local SDP".to_owned())?,
        fp.ok_or_else(|| "missing sha-256 fingerprint in local SDP".to_owned())?,
    ))
}

/// Media section parsed from a local str0m SDP offer: `(kind, mid, answer_direction)`.
#[derive(Clone, Debug)]
struct OfferSection {
    kind: String,
    mid: String,
    answer_dir: &'static str,
}

/// Extracts all `m=` sections (`audio`, `video`, `application`) with their `a=mid:` and mirrored direction.
fn parse_offer_sections(sdp: &str) -> Vec<OfferSection> {
    let mut out: Vec<OfferSection> = Vec::new();
    let mut cur_kind: Option<String> = None;
    let mut cur_mid: Option<String> = None;
    let mut cur_dir: &'static str = "sendrecv";
    let flush = |out: &mut Vec<OfferSection>, kind: &mut Option<String>, mid: &mut Option<String>, dir: &mut &'static str| {
        if let (Some(k), Some(m)) = (kind.take(), mid.take()) {
            out.push(OfferSection { kind: k, mid: m, answer_dir: *dir });
        }
        *dir = "sendrecv";
    };
    for line in sdp.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("m=") {
            flush(&mut out, &mut cur_kind, &mut cur_mid, &mut cur_dir);
            cur_kind = rest.split_whitespace().next().map(str::to_owned);
        } else if let Some(mid) = line.strip_prefix("a=mid:") {
            cur_mid = Some(mid.to_owned());
        } else if line == "a=sendonly" {
            cur_dir = "recvonly";
        } else if line == "a=recvonly" {
            cur_dir = "sendonly";
        } else if line == "a=inactive" {
            cur_dir = "inactive";
        } else if line == "a=sendrecv" {
            cur_dir = "sendrecv";
        }
    }
    flush(&mut out, &mut cur_kind, &mut cur_mid, &mut cur_dir);
    out
}

/// Builds a WebRTC SDP answer from Telegram's `updateGroupCallConnection` transport JSON
/// (supporting audio, H.264 video, and SCTP data channel sections for `#461` and `#467`).
pub fn telegram_json_to_sdp_answer(offer_sdp: &str, params_json: &str) -> Result<(String, bool), String> {
    let val: Value = serde_json::from_str(params_json).map_err(|e| format!("invalid transport JSON: {e}"))?;
    let fake_loopback = val.get("fake_loopback").and_then(Value::as_bool).unwrap_or(false)
        && crate::out::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some();

    let transport = val.get("transport").ok_or_else(|| "missing transport object".to_owned())?;
    let ufrag = transport.get("ufrag").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("missing transport.ufrag")?;
    let pwd = transport.get("pwd").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("missing transport.pwd")?;
    let fp_obj = transport
        .get("fingerprints")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .ok_or("missing transport.fingerprints")?;
    let fp_hash = fp_obj.get("hash").and_then(Value::as_str).unwrap_or("sha-256");
    let fp_val = fp_obj.get("fingerprint").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("missing fingerprint value")?;
    let setup = match fp_obj.get("setup").and_then(Value::as_str).unwrap_or("passive") {
        "active" => "active",
        _ => "passive",
    };

    let mut candidates = Vec::new();
    if let Some(arr) = transport.get("candidates").and_then(Value::as_array) {
        for c in arr {
            let ip = c.get("ip").and_then(Value::as_str).unwrap_or("");
            let port = c
                .get("port")
                .and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())))
                .unwrap_or_default();
            let proto = c.get("protocol").and_then(Value::as_str).unwrap_or("udp");
            if ip.is_empty() || port.is_empty() || !proto.eq_ignore_ascii_case("udp") {
                continue;
            }
            let foundation = c.get("foundation").and_then(Value::as_str).unwrap_or("1");
            let component = c.get("component").and_then(Value::as_str).unwrap_or("1");
            let priority = c
                .get("priority")
                .and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())))
                .unwrap_or_else(|| "2130706431".to_owned());
            let typ = c.get("type").and_then(Value::as_str).unwrap_or("host");
            candidates.push(format!("a=candidate:{foundation} {component} udp {priority} {ip} {port} typ {typ}"));
        }
    }

    let opus_pt = val
        .get("audio")
        .and_then(|a| a.get("payload-types"))
        .and_then(Value::as_array)
        .and_then(|pts| {
            pts.iter().find_map(|pt| {
                let name = pt.get("name").and_then(Value::as_str)?;
                name.eq_ignore_ascii_case("opus").then(|| pt.get("id").and_then(Value::as_u64)).flatten()
            })
        })
        .unwrap_or(111);

    let h264_pt = val
        .get("video")
        .and_then(|v| v.get("payload-types"))
        .and_then(Value::as_array)
        .and_then(|pts| {
            pts.iter().find_map(|pt| {
                let name = pt.get("name").and_then(Value::as_str)?;
                name.eq_ignore_ascii_case("h264").then(|| pt.get("id").and_then(Value::as_u64)).flatten()
            })
        })
        .unwrap_or(u64::from(TG_H264_PT));

    let sections = parse_offer_sections(offer_sdp);
    if sections.is_empty() {
        return Err("local offer had no media lines".to_owned());
    }
    let mids: Vec<&str> = sections.iter().map(|s| s.mid.as_str()).collect();

    let mut sdp = String::new();
    sdp.push_str("v=0\r\n");
    sdp.push_str("o=- 1 2 IN IP4 127.0.0.1\r\n");
    sdp.push_str("s=-\r\n");
    sdp.push_str("t=0 0\r\n");
    sdp.push_str(&format!("a=group:BUNDLE {}\r\n", mids.join(" ")));
    sdp.push_str("a=ice-lite\r\n");
    sdp.push_str("a=msid-semantic: WMS *\r\n");

    for (idx, sec) in sections.iter().enumerate() {
        match sec.kind.as_str() {
            "audio" => {
                sdp.push_str(&format!("m=audio 9 UDP/TLS/RTP/SAVPF {opus_pt}\r\n"));
                sdp.push_str("c=IN IP4 0.0.0.0\r\n");
                sdp.push_str(&format!("a=mid:{}\r\n", sec.mid));
                sdp.push_str(&format!("a=ice-ufrag:{ufrag}\r\n"));
                sdp.push_str(&format!("a=ice-pwd:{pwd}\r\n"));
                sdp.push_str(&format!("a=fingerprint:{fp_hash} {fp_val}\r\n"));
                sdp.push_str(&format!("a=setup:{setup}\r\n"));
                sdp.push_str("a=rtcp-mux\r\n");
                sdp.push_str(&format!("a={}\r\n", sec.answer_dir));
                sdp.push_str(&format!("a=rtpmap:{opus_pt} opus/48000/2\r\n"));
                sdp.push_str(&format!("a=fmtp:{opus_pt} minptime=10;useinbandfec=1\r\n"));
            }
            "video" => {
                sdp.push_str(&format!("m=video 9 UDP/TLS/RTP/SAVPF {h264_pt}\r\n"));
                sdp.push_str("c=IN IP4 0.0.0.0\r\n");
                sdp.push_str(&format!("a=mid:{}\r\n", sec.mid));
                sdp.push_str(&format!("a=ice-ufrag:{ufrag}\r\n"));
                sdp.push_str(&format!("a=ice-pwd:{pwd}\r\n"));
                sdp.push_str(&format!("a=fingerprint:{fp_hash} {fp_val}\r\n"));
                sdp.push_str(&format!("a=setup:{setup}\r\n"));
                sdp.push_str("a=rtcp-mux\r\n");
                sdp.push_str(&format!("a={}\r\n", sec.answer_dir));
                sdp.push_str(&format!("a=rtpmap:{h264_pt} H264/90000\r\n"));
                sdp.push_str(&format!(
                    "a=fmtp:{h264_pt} level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n"
                ));
            }
            "application" => {
                sdp.push_str("m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n");
                sdp.push_str("c=IN IP4 0.0.0.0\r\n");
                sdp.push_str(&format!("a=mid:{}\r\n", sec.mid));
                sdp.push_str(&format!("a=ice-ufrag:{ufrag}\r\n"));
                sdp.push_str(&format!("a=ice-pwd:{pwd}\r\n"));
                sdp.push_str(&format!("a=fingerprint:{fp_hash} {fp_val}\r\n"));
                sdp.push_str(&format!("a=setup:{setup}\r\n"));
                sdp.push_str("a=sctp-port:5000\r\n");
            }
            _ => continue,
        }
        if idx == 0 {
            for cand in &candidates {
                sdp.push_str(cand);
                sdp.push_str("\r\n");
            }
        }
    }

    Ok((sdp, fake_loopback))
}

/// Builds Telegram's Colibri `ReceiverVideoConstraints` JSON sent on the group call data channel
/// to request forwarded video/presentation streams and their resolution tiers (`#467`).
pub fn build_video_constraints_json(
    sources: &[TelegramGroupVideoSource],
    watch: &[WatchSource],
) -> String {
    let mut on_stage: Vec<&str> = Vec::new();
    let mut constraints = serde_json::Map::new();
    for want in watch {
        if let Some(src) = sources
            .iter()
            .find(|s| s.user_id == want.user_id && s.kind == want.kind && !s.paused && !s.endpoint.is_empty())
        {
            let (min_h, max_h) = match want.size {
                WatchSize::Large => {
                    if !on_stage.contains(&src.endpoint.as_str()) {
                        on_stage.push(src.endpoint.as_str());
                    }
                    (360, 720)
                }
                WatchSize::Small => (180, 360),
            };
            constraints.insert(
                src.endpoint.clone(),
                json!({ "minHeight": min_h, "maxHeight": max_h }),
            );
        }
    }
    json!({
        "colibriClass": "ReceiverVideoConstraints",
        "onStageEndpoints": on_stage,
        "defaultConstraints": { "maxHeight": 0 },
        "constraints": constraints,
    })
    .to_string()
}

/// An outgoing Telegram group voice/video chat offer, awaiting `updateGroupCallConnection`.
pub struct GroupOffer {
    rtc: Rtc,
    udp: MioUdp,
    local_udp: SocketAddr,
    pending: SdpPendingOffer,
    offer_sdp: String,
    params_json: String,
    ssrc: u32,
    mic_mid: Mid,
    recv_mids: Vec<Mid>,
    cam_mid: Mid,
    video_recv_mids: Vec<Mid>,
}

/// An accepted Telegram group voice/video chat transport ready for [`run_group`].
pub struct GroupLink {
    rtc: Rtc,
    udp: MioUdp,
    local_udp: SocketAddr,
    ssrc: u32,
    mic_mid: Mid,
    recv_mids: Vec<Mid>,
    cam_mid: Mid,
    video_recv_mids: Vec<Mid>,
    fake_loopback: bool,
}

impl GroupOffer {
    pub fn new(_spec: &TelegramGroupOffer) -> io::Result<GroupOffer> {
        let ip = local_ip();
        let mut rtc = group_rtc_config().build(Instant::now());
        let udp = MioUdp::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
        let local_udp = SocketAddr::new(ip, udp.local_addr()?.port());
        let cand = Candidate::host(local_udp, "udp").map_err(|e| io::Error::other(format!("candidate: {e:?}")))?;
        rtc.add_local_candidate(cand);

        let mut change = rtc.sdp_api();
        let mic_mid = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let mut recv_mids = Vec::with_capacity(GROUP_RECV_SLOTS);
        for _ in 0..GROUP_RECV_SLOTS {
            recv_mids.push(change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None));
        }
        let cam_mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let mut video_recv_mids = Vec::with_capacity(GROUP_VIDEO_RECV_SLOTS);
        for _ in 0..GROUP_VIDEO_RECV_SLOTS {
            video_recv_mids.push(change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None));
        }
        let _dc_id = change.add_channel("data".to_owned());

        let (offer, pending) = change.apply().ok_or_else(|| io::Error::other("nothing to offer"))?;
        let offer_sdp = offer.to_sdp_string();
        let (ufrag, pwd, fingerprint) = parse_offer_ice_dtls(&offer_sdp).map_err(io::Error::other)?;
        // Positive 31-bit SSRCs so they map cleanly to Telegram's signed `i32` fields.
        let mut rng = rand::thread_rng();
        let ssrc: u32 = rng.gen_range(1..=0x7fff_ffffu32);
        let video_ssrc: u32 = rng.gen_range(1..=0x7fff_ffffu32);

        let params_json = json!({
            "ufrag": ufrag,
            "pwd": pwd,
            "fingerprints": [
                {
                    "hash": "sha-256",
                    "setup": "active",
                    "fingerprint": fingerprint,
                }
            ],
            "ssrc": ssrc as i32,
            "ssrc-groups": [
                {
                    "semantics": "SIM",
                    "sources": [video_ssrc as i32],
                }
            ],
        })
        .to_string();

        Ok(GroupOffer {
            rtc,
            udp,
            local_udp,
            pending,
            offer_sdp,
            params_json,
            ssrc,
            mic_mid,
            recv_mids,
            cam_mid,
            video_recv_mids,
        })
    }

    pub fn params_json(&self) -> &str {
        &self.params_json
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn offer_sdp(&self) -> &str {
        &self.offer_sdp
    }

    pub fn accept(mut self, params_json: &str) -> Result<GroupLink, String> {
        let (answer_sdp, fake_loopback) = telegram_json_to_sdp_answer(&self.offer_sdp, params_json)?;
        let answer = SdpAnswer::from_sdp_string(&answer_sdp).map_err(|e| format!("sdp parse: {e:?}"))?;
        self.rtc.sdp_api().accept_answer(self.pending, answer).map_err(|e| format!("accept_answer: {e:?}"))?;
        Ok(GroupLink {
            rtc: self.rtc,
            udp: self.udp,
            local_udp: self.local_udp,
            ssrc: self.ssrc,
            mic_mid: self.mic_mid,
            recv_mids: self.recv_mids,
            cam_mid: self.cam_mid,
            video_recv_mids: self.video_recv_mids,
            fake_loopback,
        })
    }
}

/// Separate screen-share presentation transport for `phone.joinGroupCallPresentation` (`#467`).
pub struct PresentationOffer {
    rtc: Rtc,
    udp: MioUdp,
    local_udp: SocketAddr,
    pending: SdpPendingOffer,
    offer_sdp: String,
    params_json: String,
    ssrc: u32,
    share_mid: Mid,
}

pub struct PresentationLink {
    rtc: Rtc,
    udp: MioUdp,
    local_udp: SocketAddr,
    #[cfg(test)]
    ssrc: u32,
    share_mid: Mid,
    fake_loopback: bool,
}

impl PresentationOffer {
    pub fn new() -> io::Result<PresentationOffer> {
        let ip = local_ip();
        let mut rtc = group_rtc_config().build(Instant::now());
        let udp = MioUdp::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
        let local_udp = SocketAddr::new(ip, udp.local_addr()?.port());
        let cand = Candidate::host(local_udp, "udp").map_err(|e| io::Error::other(format!("candidate: {e:?}")))?;
        rtc.add_local_candidate(cand);

        let mut change = rtc.sdp_api();
        let share_mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().ok_or_else(|| io::Error::other("nothing to offer"))?;
        let offer_sdp = offer.to_sdp_string();
        let (ufrag, pwd, fingerprint) = parse_offer_ice_dtls(&offer_sdp).map_err(io::Error::other)?;
        let ssrc: u32 = rand::thread_rng().gen_range(1..=0x7fff_ffffu32);

        let params_json = json!({
            "ufrag": ufrag,
            "pwd": pwd,
            "fingerprints": [
                {
                    "hash": "sha-256",
                    "setup": "active",
                    "fingerprint": fingerprint,
                }
            ],
            "ssrc": ssrc as i32,
            "ssrc-groups": [
                {
                    "semantics": "SIM",
                    "sources": [ssrc as i32],
                }
            ],
        })
        .to_string();

        Ok(PresentationOffer {
            rtc,
            udp,
            local_udp,
            pending,
            offer_sdp,
            params_json,
            ssrc,
            share_mid,
        })
    }

    pub fn params_json(&self) -> &str {
        &self.params_json
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn accept(mut self, params_json: &str) -> Result<PresentationLink, String> {
        let (answer_sdp, fake_loopback) = telegram_json_to_sdp_answer(&self.offer_sdp, params_json)?;
        let answer = SdpAnswer::from_sdp_string(&answer_sdp).map_err(|e| format!("sdp parse: {e:?}"))?;
        self.rtc.sdp_api().accept_answer(self.pending, answer).map_err(|e| format!("accept_answer: {e:?}"))?;
        Ok(PresentationLink {
            rtc: self.rtc,
            udp: self.udp,
            local_udp: self.local_udp,
            #[cfg(test)]
            ssrc: self.ssrc,
            share_mid: self.share_mid,
            fake_loopback,
        })
    }
}

/// Runs the separate Telegram presentation (screen-share) transport until stopped (`#467`).
pub fn run_presentation(mut link: PresentationLink, controls: Arc<Controls>) {
    let mut poll = match Poll::new() {
        Ok(p) => p,
        Err(_) => return,
    };
    if poll.registry().register(&mut link.udp, UDP_TOKEN, Interest::READABLE).is_err() {
        return;
    }
    let mut video = VideoEngine::new(controls.clone());
    if !link.fake_loopback {
        video.want(VideoWants { share: true, ..VideoWants::default() });
    }
    let mut events = Events::with_capacity(16);
    let mut recv_buf = [0u8; 2048];

    while !controls.hangup.load(Ordering::Relaxed)
        && !controls.tg_presentation_stopped()
        && controls.share()
        && !out::over()
    {
        if !link.fake_loopback {
            loop {
                if !link.rtc.is_alive() {
                    return;
                }
                match link.rtc.poll_output() {
                    Ok(Output::Timeout(_)) => break,
                    Ok(Output::Transmit(tx)) => {
                        if tx.proto == Protocol::Udp {
                            let _ = link.udp.send_to(&tx.contents, tx.destination);
                        }
                    }
                    Ok(Output::Event(Event::KeyframeRequest(_))) => {
                        video.ask_key(VideoTrack::Share);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            while let Some(word) = video.next() {
                match word {
                    VideoWord::Frame { track: VideoTrack::Share, payload, rtp, .. } => {
                        if let Some(writer) = link.rtc.writer(link.share_mid) {
                            let pt = writer.payload_params().next().map(|p| p.pt());
                            if let Some(pt) = pt {
                                let time = MediaTime::new(rtp, Frequency::NINETY_KHZ);
                                let _ = writer.write(pt, Instant::now(), time, payload);
                            }
                        }
                    }
                    VideoWord::Failed { words, .. } => {
                        out::send(&MediaResponse::Notice { detail: words });
                        controls.set_share(false);
                        return;
                    }
                    _ => {}
                }
            }
        }
        let _ = poll.poll(&mut events, Some(Duration::from_millis(20)));
        if !link.fake_loopback {
            for ev in events.iter() {
                if ev.token() == UDP_TOKEN {
                    loop {
                        match link.udp.recv_from(&mut recv_buf) {
                            Ok((n, src)) => {
                                if let Ok(contents) = (&recv_buf[..n]).try_into() {
                                    let recv = Receive {
                                        proto: Protocol::Udp,
                                        source: src,
                                        destination: link.local_udp,
                                        contents,
                                    };
                                    let _ = link.rtc.handle_input(Input::Receive(Instant::now(), recv));
                                }
                            }
                            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                            Err(_) => break,
                        }
                    }
                }
            }
            let _ = link.rtc.handle_input(Input::Timeout(Instant::now()));
        }
    }
    video.want(VideoWants::default());
}

/// Bounds a map of remote voice streams to at most `cap` loudest/most-recent active speakers (`#461`).
pub(crate) fn enforce_speaker_cap<K: Eq + std::hash::Hash + Clone>(
    streams: &mut HashMap<K, (String, u8, Instant, Stream)>,
    cap: usize,
) -> Vec<String> {
    let mut silenced = Vec::new();
    while streams.len() > cap {
        let victim = streams
            .iter()
            .min_by_key(|(_, (_, energy, last, _))| (*energy, *last))
            .map(|(k, _)| k.clone());
        let Some(key) = victim else { break };
        if let Some((user_id, _, _, stream)) = streams.remove(&key) {
            if stream.speaking() && !user_id.is_empty() {
                silenced.push(user_id);
            }
        }
    }
    silenced
}

/// Runs a Telegram group voice chat (`#461`).
pub fn run_group(mut link: GroupLink, user_id: String, controls: Arc<Controls>) {
    rt::tick_thread();
    let _timer = rt::FineTimer::hold();

    let Some(mut encoder) = Encoder::voice() else {
        out::fail(CallFailure::Engine, ENGINE);
    };

    let mut poll = match Poll::new() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!("mediad: poll failed: {err}");
            out::fail(CallFailure::Engine, ENGINE);
        }
    };
    if let Err(err) = poll.registry().register(&mut link.udp, UDP_TOKEN, Interest::READABLE) {
        tracing::warn!("mediad: udp register failed: {err}");
        out::fail(CallFailure::Engine, ENGINE);
    }

    let microphone_unasked = matches!(microphone(), Some(Microphone::Unasked));
    let mut audio: Option<Audio> = None;
    let mut devices: Option<Devices> = None;
    let mut connected = false;
    let started = Instant::now();

    let mut gate = Gate::default();
    let mut processor = Processor::new();
    let mut limiter = Limiter::default();
    let mut meter = Meter::new(CallPath::Udp);
    let mut cue: Option<Playing> = None;

    let mut ssrc_map: HashMap<u32, String> = HashMap::new();
    let mut video_sources: Vec<TelegramGroupVideoSource> = Vec::new();
    let mut data_channel: Option<ChannelId> = None;
    let mut last_constraints = String::new();
    let mut video = VideoEngine::new(controls.clone());
    let mut last_sending = (false, false);
    let mut streams: HashMap<Mid, (String, u8, Instant, Stream)> = HashMap::new();
    let mut recv_packets: VecDeque<(SocketAddr, Vec<u8>)> = VecDeque::new();
    let mut events = Events::with_capacity(32);
    let mut recv_buf = [0u8; 2048];
    let mut opus_buf = [0u8; 1275];
    let mut ticks = 0u64;
    let mut levels = (0u8, 0u8);
    let mut tick_at = Instant::now() + TICK;

    let open_audio = |audio: &mut Option<Audio>, devices: &mut Option<Devices>| {
        if audio.is_some() {
            return;
        }
        let (input, output) = controls.take_chosen();
        match Audio::open(input, output) {
            Ok(opened) => {
                announce(&opened);
                *audio = Some(opened);
                *devices = Some(Devices::start(microphone_unasked));
            }
            Err(err) => {
                let (reason, detail) = media::failure(err);
                out::fail(reason, detail);
            }
        }
    };

    if link.fake_loopback {
        open_audio(&mut audio, &mut devices);
        connected = true;
        out::send(&MediaResponse::Connected { ssrc: link.ssrc, dave_version: 0 });
    }

    while !controls.hangup.load(Ordering::Relaxed) && !out::over() {
        // Apply updated SSRC -> participant mappings from chatd.
        if let Some(sources) = controls.take_tg_sources() {
            ssrc_map.clear();
            for TelegramGroupSource { ssrc, user_id: uid } in sources {
                ssrc_map.insert(ssrc, uid);
            }
            let ordered: Vec<String> = ssrc_map.values().cloned().collect();
            for (idx, mid) in link.recv_mids.iter().enumerate() {
                if let Some((slot_uid, _, _, _)) = streams.get_mut(mid) {
                    if let Some(uid) = ordered.get(idx) {
                        *slot_uid = uid.clone();
                    }
                }
            }
        }

        // Apply updated remote video & presentation endpoints/SSRCs from chatd (`#467`).
        if let Some(vsrcs) = controls.take_tg_video_sources() {
            video_sources = vsrcs;
        }
        // Sync camera/share sending state and data-channel video constraints (`#467`).
        let want_cam = controls.camera();
        let want_share = controls.share();
        let want_preview = controls.camera_preview();
        if !link.fake_loopback {
            video.want(VideoWants {
                share: false,
                camera: want_cam,
                preview_camera: want_preview,
                ..VideoWants::default()
            });
        }
        if (want_share, want_cam) != last_sending {
            last_sending = (want_share, want_cam);
            out::send(&MediaResponse::Sending { share: want_share, camera: want_cam });
        }
        let watch_list = controls.watch_sources();
        for (slot_idx, want) in watch_list.iter().take(GROUP_VIDEO_RECV_SLOTS).enumerate() {
            video.watch_slot(slot_idx, want.size == WatchSize::Small);
        }
        if !link.fake_loopback {
            if let Some(cid) = data_channel {
                let next_constraints = build_video_constraints_json(&video_sources, &watch_list);
                if next_constraints != last_constraints {
                    if let Some(mut ch) = link.rtc.channel(cid) {
                        let _ = ch.write(false, next_constraints.as_bytes());
                        last_constraints = next_constraints;
                    }
                }
            }
            while let Some(word) = video.next() {
                match word {
                    VideoWord::Frame { track: VideoTrack::High, payload, rtp, .. } => {
                        if let Some(writer) = link.rtc.writer(link.cam_mid) {
                            let pt = writer.payload_params().next().map(|p| p.pt());
                            if let Some(pt) = pt {
                                let time = MediaTime::new(rtp, Frequency::NINETY_KHZ);
                                let _ = writer.write(pt, Instant::now(), time, payload);
                            }
                        }
                    }
                    VideoWord::Failed { words, share } => {
                        out::send(&MediaResponse::Notice { detail: words });
                        if share {
                            controls.set_share(false);
                        } else {
                            controls.set_camera(false);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Drain str0m outputs (transmits & events).
        let mut next_timeout = None;
        if !link.fake_loopback {
            loop {
                if !link.rtc.is_alive() {
                    out::fail(CallFailure::Lost, "The Telegram group voice connection closed.");
                }
                match link.rtc.poll_output() {
                    Ok(Output::Timeout(at)) => {
                        next_timeout = Some(at);
                        break;
                    }
                    Ok(Output::Transmit(tx)) => {
                        if tx.proto == Protocol::Udp && link.udp.send_to(&tx.contents, tx.destination).is_ok() {
                            meter.sent(tx.contents.len());
                        }
                    }
                    Ok(Output::Event(ev)) => match ev {
                        Event::Connected => {
                            if !connected {
                                open_audio(&mut audio, &mut devices);
                                connected = true;
                                out::send(&MediaResponse::Connected { ssrc: link.ssrc, dave_version: 0 });
                            }
                        }
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                            if connected {
                                out::send(&MediaResponse::Interrupted);
                            }
                        }
                        Event::ChannelOpen(cid, _) => {
                            data_channel = Some(cid);
                            last_constraints.clear();
                        }
                        Event::KeyframeRequest(_) => {
                            video.ask_key(VideoTrack::High);
                        }
                        Event::MediaData(data) => {
                            let deafened = controls.deafened.load(Ordering::Relaxed);
                            if let Some(vslot) = link.video_recv_mids.iter().position(|m| *m == data.mid) {
                                if !deafened {
                                    let mapped = watch_list
                                        .get(vslot)
                                        .map(|w| (w.user_id.clone(), w.kind))
                                        .or_else(|| {
                                            video_sources
                                                .get(vslot)
                                                .map(|s| (s.user_id.clone(), s.kind))
                                        });
                                    if let Some((vuid, kind)) = mapped {
                                        let key = is_h264_keyframe(&data.data);
                                        let _ = video.picture_slot(
                                            vslot,
                                            VideoReceived {
                                                codec: VideoCodec::H264,
                                                data: Arc::<[u8]>::from(data.data),
                                                key,
                                                contiguous: data.contiguous,
                                                user: vuid,
                                                kind,
                                            },
                                        );
                                    }
                                }
                                continue;
                            }
                            let now = Instant::now();
                            // Encrypted conference calls: fail closed — drop any frame that fails
                            // per-frame AEAD decryption under the verified chain epoch key.
                            let payload = if controls.tg_conference_active() {
                                match controls.tg_conference_decrypt(&data.data) {
                                    Ok((_kind, _sender, plain)) => plain,
                                    Err(_) => continue,
                                }
                            } else {
                                data.data.to_vec()
                            };
                            let slot_idx = link.recv_mids.iter().position(|m| *m == data.mid).unwrap_or(0);
                            let uid = ssrc_map
                                .values()
                                .nth(slot_idx)
                                .cloned()
                                .unwrap_or_else(|| format!("ssrc:{slot_idx}"));
                            if !streams.contains_key(&data.mid) {
                                if let Some(stream) = Stream::clocked() {
                                    streams.insert(data.mid, (uid, 0, now, stream));
                                    for quiet_uid in enforce_speaker_cap(&mut streams, MAX_ACTIVE_SPEAKERS) {
                                        out::send(&MediaResponse::Speaking { user_id: quiet_uid, speaking: false });
                                    }
                                }
                            }
                            if let Some((_, _, last, stream)) = streams.get_mut(&data.mid) {
                                *last = now;
                                let ts = data.time.numer();
                                stream.last_packet = now;
                                stream.arrivals.arrived(ts as u32, now);
                                stream.heard |= payload[..] != SILENCE[..];
                                if !deafened {
                                    stream.took(Duration::ZERO);
                                    stream.jitter.insert((ts / FRAME as u64) as u16, payload);
                                }
                            }
                        }
                        _ => {}
                    },
                    Err(_) => break,
                }
            }
            if !connected && started.elapsed() > CONNECT_TIMEOUT {
                out::fail(CallFailure::Lost, NO_AUDIO_PATH);
            }
        }

        // Poll UDP until next tick or str0m timeout.
        let now = Instant::now();
        let wake = match next_timeout {
            Some(t) if t < tick_at => t,
            _ => tick_at,
        };
        let wait = wake.saturating_duration_since(now).min(Duration::from_millis(10));
        if poll.poll(&mut events, Some(wait)).is_ok() {
            for ev in events.iter() {
                if ev.token() == UDP_TOKEN {
                    loop {
                        match link.udp.recv_from(&mut recv_buf) {
                            Ok((n, src)) => {
                                meter.received(n);
                                recv_packets.push_back((src, recv_buf[..n].to_vec()));
                            }
                            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                            Err(_) => break,
                        }
                    }
                }
            }
        }

        if !link.fake_loopback {
            while let Some((src, pkt)) = recv_packets.pop_front() {
                if let Ok(contents) = pkt.as_slice().try_into() {
                    let recv = Receive {
                        proto: Protocol::Udp,
                        source: src,
                        destination: link.local_udp,
                        contents,
                    };
                    let _ = link.rtc.handle_input(Input::Receive(Instant::now(), recv));
                }
            }
            let _ = link.rtc.handle_input(Input::Timeout(Instant::now()));
        } else {
            recv_packets.clear();
        }

        let now = Instant::now();
        if now < tick_at {
            continue;
        }
        tick_at = next_tick(tick_at, now);
        ticks += 1;

        let Some(audio_ref) = audio.as_mut() else {
            continue;
        };
        if let Some(devs) = devices.as_mut() {
            if ticks % SECOND == 0 {
                if let Err((reason, detail)) = devs.permission() {
                    out::fail(reason, detail);
                }
            }
            if let Err((reason, detail)) = devs.service(audio_ref, &controls, ticks) {
                out::fail(reason, detail);
            }
        }

        encoder.set_bitrate(controls.bitrate());

        // Capture microphone and send Opus over str0m when unmuted.
        let mut frame = [0i16; FRAME_SAMPLES];
        let (step, input_level) = listen(&controls, audio_ref, &mut processor, &mut gate, &mut frame);
        if let Some(speaking) = step.speaking {
            out::send(&MediaResponse::Speaking { user_id: user_id.clone(), speaking });
        }

        if !link.fake_loopback && !controls.muted.load(Ordering::Relaxed) {
            let conf_e2ee = controls.tg_conference_active();
            let mut send_opus = |data: &[u8], tick: u64| {
                // Encrypted conference calls: NEVER transmit plaintext if chain is pending or blocked.
                let sealed_buf;
                let wire: &[u8] = if conf_e2ee {
                    match controls.tg_conference_encrypt(telegram_e2ee::FRAME_KIND_AUDIO, data) {
                        Ok(ct) => {
                            sealed_buf = ct;
                            &sealed_buf
                        }
                        Err(_) => return,
                    }
                } else {
                    data
                };
                if let Some(writer) = link.rtc.writer(link.mic_mid) {
                    let pt = writer.payload_params().next().map(|p| p.pt());
                    if let Some(pt) = pt {
                        let time = MediaTime::new(tick * FRAME as u64, Frequency::FORTY_EIGHT_KHZ);
                        let _ = writer.write(pt, Instant::now(), time, wire);
                    }
                }
            };
            for early in step.early {
                if let Some(len) = encoder.encode(&early, &mut opus_buf) {
                    send_opus(&opus_buf[..len], ticks);
                }
            }
            if let Some(send) = step.send {
                match send {
                    GateSend::Frame(f) => {
                        if let Some(len) = encoder.encode(&f, &mut opus_buf) {
                            send_opus(&opus_buf[..len], ticks);
                        }
                    }
                    GateSend::Silence => send_opus(&SILENCE, ticks),
                }
            }
        }

        // Mix up to MAX_ACTIVE_SPEAKERS active remote voices.
        let deafened = controls.deafened.load(Ordering::Relaxed);
        let ear = controls.ear();
        let ducked = ear.ducking(streams.values().map(|(uid, _, _, s)| (uid.as_str(), s.speaking())));
        let mut mix = [0i32; FRAME_SAMPLES];
        for (_, (uid, energy, _, stream)) in streams.iter_mut() {
            let loud = if deafened {
                stream.drop_audio();
                stream.heard
            } else if let Some(pcm) = stream.pull(uid) {
                let lvl = level(&pcm);
                *energy = lvl;
                stream.mix_into(&mut mix, &pcm, ear.factor(uid, ducked));
                lvl >= VOICE
            } else {
                *energy = energy.saturating_sub(4);
                false
            };
            if let Some(speaking) = stream.hear(loud) {
                if !uid.is_empty() && !uid.starts_with("ssrc:") {
                    out::send(&MediaResponse::Speaking { user_id: uid.clone(), speaking });
                }
            }
        }
        let output_level = mixdown(&mix, &controls, &mut cue, audio_ref, &mut processor, &mut limiter);
        levels = (levels.0.max(input_level), levels.1.max(output_level));

        if ticks % 5 == 0 && controls.metering() {
            out::send(&MediaResponse::Levels { input: levels.0, output: levels.1 });
            levels = (0, 0);
        }
        if ticks % SECOND == 0 {
            let stats = meter.second(streams.values_mut().map(|(_, _, _, s)| s.counts()));
            controls.publish_stats(stats);
        }
    }

    if let Some(mut audio_ref) = audio {
        finish_cue(&controls, &mut cue, &mut audio_ref);
    }
}

// ---------------------------------------------------------------------------
// Channel RTMP Live Streams (`#454`)
// ---------------------------------------------------------------------------

/// One fetched `inputGroupCallStream` segment handed from `chatd` to `mediad`.
#[derive(Clone, Debug)]
pub struct RtmpSegmentInput {
    pub time_ms: i64,
    pub scale: i32,
    pub video_channel: Option<i32>,
    pub video_quality: Option<u8>,
    pub bytes: Vec<u8>,
}

/// Decoded audio (Opus packets) and video (H.264 Annex-B access units) extracted from an RTMP segment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RtmpSegmentMedia {
    pub opus_frames: Vec<Vec<u8>>,
    pub h264_units: Vec<Vec<u8>>,
    pub box_types: Vec<[u8; 4]>,
}

/// Parses a fetched Telegram `inputGroupCallStream` segment (ISO-BMFF/fMP4 boxes or framed `TGAV`
/// audio/video chunks) without panicking on truncated or unknown payloads (`#454`).
pub fn parse_rtmp_segment(bytes: &[u8]) -> RtmpSegmentMedia {
    let mut out = RtmpSegmentMedia::default();
    if bytes.is_empty() {
        return out;
    }
    // Framed TGAV test/passthrough container: "TGAV" + repeated (kind: u8 [1=opus, 2=h264], len: u32 BE, payload).
    if bytes.starts_with(b"TGAV") {
        let mut pos = 4;
        while pos + 5 <= bytes.len() {
            let kind = bytes[pos];
            let len = u32::from_be_bytes([bytes[pos + 1], bytes[pos + 2], bytes[pos + 3], bytes[pos + 4]]) as usize;
            pos += 5;
            if pos + len > bytes.len() {
                break;
            }
            let payload = bytes[pos..pos + len].to_vec();
            match kind {
                1 if !payload.is_empty() => out.opus_frames.push(payload),
                2 if !payload.is_empty() => out.h264_units.push(payload),
                _ => {}
            }
            pos += len;
        }
        return out;
    }

    // Standard ISO-BMFF / fMP4 box walk (`ftyp`, `moov`, `moof`, `mdat`, ...).
    let mut pos = 0;
    while pos + 8 <= bytes.len() {
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        let box_type = [bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]];
        if size < 8 || pos + size > bytes.len() {
            break;
        }
        out.box_types.push(box_type);
        let body = &bytes[pos + 8..pos + size];
        if &box_type == b"mdat" && !body.is_empty() {
            // Look for Annex-B H.264 NALUs or length-prefixed AVCC NALUs; otherwise treat small payloads as Opus.
            if body.windows(4).any(|w| w == [0, 0, 0, 1]) {
                out.h264_units.push(body.to_vec());
            } else if body.len() <= 1275 {
                out.opus_frames.push(body.to_vec());
            }
        }
        pos += size;
    }
    out
}

/// Runs a receive-only Telegram channel RTMP live stream viewer in `mediad` (`#454`).
/// Opens audio playback (never capturing a microphone), decodes incoming segment audio & video,
/// and honors pause/end/deafen state cleanly.
pub fn run_rtmp_viewer(channel_id: String, controls: Arc<Controls>) {
    rt::tick_thread();
    let _timer = rt::FineTimer::hold();

    let (input, output) = controls.take_chosen();
    let mut audio = match Audio::open(input, output) {
        Ok(opened) => {
            announce(&opened);
            opened
        }
        Err(err) => {
            let (reason, detail) = media::failure(err);
            out::fail(reason, detail);
        }
    };
    let mut decoder = OpusDecoder::new();
    let mut video = VideoEngine::new(controls.clone());
    video.watch_slot(0, false);
    let mut processor = Processor::new();
    let mut limiter = Limiter::default();
    let mut cue: Option<Playing> = None;
    let mut opus_queue: VecDeque<Vec<u8>> = VecDeque::new();
    let mut tick_at = Instant::now() + TICK;

    out::send(&MediaResponse::Connected { ssrc: 0, dave_version: 0 });

    while !controls.hangup.load(Ordering::Relaxed) && !out::over() {
        let state = controls.tg_rtmp_state().unwrap_or(RtmpStreamState::Waiting);
        for seg in controls.take_tg_rtmp_segments() {
            let parsed = parse_rtmp_segment(&seg.bytes);
            for frame in parsed.opus_frames {
                if opus_queue.len() < 128 {
                    opus_queue.push_back(frame);
                }
            }
            for unit in parsed.h264_units {
                let key = is_h264_keyframe(&unit);
                let _ = video.picture_slot(
                    0,
                    VideoReceived {
                        codec: VideoCodec::H264,
                        data: Arc::from(unit.into_boxed_slice()),
                        key,
                        contiguous: true,
                        user: channel_id.clone(),
                        kind: PictureKind::Share,
                    },
                );
            }
        }
        if matches!(state, RtmpStreamState::Paused | RtmpStreamState::Ended) {
            opus_queue.clear();
        }

        let now = Instant::now();
        if now < tick_at {
            std::thread::sleep(tick_at.saturating_duration_since(now).min(Duration::from_millis(10)));
            continue;
        }
        tick_at = next_tick(tick_at, now);

        let deafened = controls.deafened.load(Ordering::Relaxed);
        let mut mix = [0i32; FRAME_SAMPLES];
        if !deafened && state == RtmpStreamState::Live {
            if let (Some(packet), Some(dec)) = (opus_queue.pop_front(), decoder.as_mut()) {
                let mut pcm = [0i16; FRAME_SAMPLES];
                if let Some(samples) = dec.decode(Some(&packet), 0, &mut pcm) {
                    let count = (samples * 2).min(FRAME_SAMPLES);
                    for i in 0..count {
                        mix[i] = i32::from(pcm[i]);
                    }
                }
            }
        }
        let _ = mixdown(&mix, &controls, &mut cue, &mut audio, &mut processor, &mut limiter);
    }

    finish_cue(&controls, &mut cue, &mut audio);
}

fn next_tick(tick_at: Instant, now: Instant) -> Instant {
    let next = tick_at + TICK;
    if now > next + 5 * TICK {
        now + TICK
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtproto_call_seal_and_open_round_trip_and_rejects_tamper() {
        let mut key = [0u8; 256];
        for (i, b) in key.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(13);
        }
        let msg = b"hello encrypted telegram voice frame";
        let sealed = seal_mtproto(&key, true, msg);
        assert_eq!(open_mtproto(&key, true, &sealed).as_deref(), Some(msg.as_slice()));
        // Wrong direction offset (`x = 8` instead of `0`) must fail authentication.
        assert!(open_mtproto(&key, false, &sealed).is_none());
        // Single bit flip in ciphertext must fail msg_key verification.
        let mut tampered = sealed.clone();
        tampered[20] ^= 0x01;
        assert!(open_mtproto(&key, true, &tampered).is_none());
        // Single bit flip in msg_key must fail verification.
        let mut bad_tag = sealed;
        bad_tag[3] ^= 0x80;
        assert!(open_mtproto(&key, true, &bad_tag).is_none());
    }

    #[test]
    fn direct_udp_reflector_loopback_carries_encrypted_opus_frames() {
        let key = [0x42u8; 256];
        let tag = [0x99u8; 16];
        let mut enc = Encoder::voice().expect("opus encoder");
        let mut dec = crate::opus::Decoder::new().expect("opus decoder");

        // Synthesize one 440 Hz stereo 20 ms frame.
        let mut pcm = [0i16; FRAME_SAMPLES];
        for i in 0..FRAME {
            let s = ((i as f64 / 48_000.0 * 440.0 * std::f64::consts::TAU).sin() * 12_000.0) as i16;
            pcm[i * 2] = s;
            pcm[i * 2 + 1] = s;
        }
        let mut opus = [0u8; 1275];
        let len = enc.encode(&pcm, &mut opus).expect("encode");

        let sock_a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let sock_b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sock_b.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        let wire = pack_direct_frame(&key, true, Some(&tag), DIRECT_KIND_OPUS, 7, 6720, &opus[..len]);
        sock_a.send_to(&wire, sock_b.local_addr().unwrap()).unwrap();

        let mut buf = [0u8; 2048];
        let (n, _) = sock_b.recv_from(&mut buf).unwrap();
        let (kind, seq, ts, payload) = unpack_direct_frame(&key, true, Some(&tag), &buf[..n]).expect("decrypt");
        assert_eq!((kind, seq, ts), (DIRECT_KIND_OPUS, 7, 6720));
        assert_eq!(payload, &opus[..len]);

        let mut decoded = [0i16; FRAME_SAMPLES];
        let samples = dec.decode(Some(&payload), 0, &mut decoded).expect("decode");
        assert_eq!(samples, FRAME);
    }

    #[test]
    fn group_offer_json_has_ufrag_pwd_fingerprint_and_positive_ssrc_and_accepts_answer() {
        let spec = TelegramGroupOffer {
            user_id: "u7001".to_owned(),
            muted: true,
            deafened: false,
            conference: false,
        };
        let offer = GroupOffer::new(&spec).expect("group offer");
        assert!(offer.ssrc() > 0 && offer.ssrc() <= i32::MAX as u32);
        let parsed: Value = serde_json::from_str(offer.params_json()).expect("valid JSON");
        assert!(!parsed["ufrag"].as_str().unwrap_or("").is_empty());
        assert!(!parsed["pwd"].as_str().unwrap_or("").is_empty());
        assert_eq!(parsed["fingerprints"][0]["hash"].as_str(), Some("sha-256"));
        assert_eq!(parsed["ssrc"].as_i64(), Some(offer.ssrc() as i64));

        // Synthesize a Telegram `updateGroupCallConnection` transport payload and accept it with str0m.
        let fake_remote_fp = parsed["fingerprints"][0]["fingerprint"].as_str().unwrap();
        let server_params = json!({
            "transport": {
                "ufrag": "tgserverufrag",
                "pwd": "tgserverpasswordlongenough1234",
                "fingerprints": [
                    { "hash": "sha-256", "setup": "passive", "fingerprint": fake_remote_fp }
                ],
                "candidates": [
                    {
                        "generation": "0",
                        "component": "1",
                        "protocol": "udp",
                        "port": "59600",
                        "ip": "127.0.0.1",
                        "foundation": "1",
                        "id": "1",
                        "priority": "2130706431",
                        "type": "host"
                    }
                ]
            },
            "audio": {
                "payload-types": [
                    { "id": 111, "name": "opus", "clockrate": 48000, "channels": 2 }
                ]
            }
        })
        .to_string();
        let link = offer.accept(&server_params).expect("str0m accepts synthesized Telegram answer");
        assert_eq!(link.ssrc, parsed["ssrc"].as_u64().unwrap() as u32);
    }

    #[test]
    fn loudest_speaker_cap_evicts_quietest_streams() {
        let now = Instant::now();
        let mut streams: HashMap<u32, (String, u8, Instant, Stream)> = HashMap::new();
        for i in 0..16u32 {
            let s = Stream::clocked().expect("stream");
            streams.insert(i, (format!("u{i}"), (i * 5) as u8, now, s));
        }
        enforce_speaker_cap(&mut streams, MAX_ACTIVE_SPEAKERS);
        assert_eq!(streams.len(), MAX_ACTIVE_SPEAKERS);
        for i in 0..4u32 {
            assert!(!streams.contains_key(&i), "quietest stream {i} should be evicted");
        }
        for i in 4..16u32 {
            assert!(streams.contains_key(&i), "louder stream {i} should stay");
        }
    }

    #[test]
    fn presentation_offer_and_video_constraints_and_rtmp_parser() {
        // Presentation offer builds valid JSON and accepts presentation transport answer (#467).
        let pres = PresentationOffer::new().expect("presentation offer");
        let expected_ssrc = pres.ssrc();
        assert!(expected_ssrc > 0);
        let parsed: Value = serde_json::from_str(pres.params_json()).unwrap();
        let fp = parsed["fingerprints"][0]["fingerprint"].as_str().unwrap();
        let server_params = json!({
            "transport": {
                "ufrag": "tgpresufrag",
                "pwd": "tgprespasswordlongenough1234",
                "fingerprints": [{ "hash": "sha-256", "setup": "passive", "fingerprint": fp }],
                "candidates": [{ "protocol": "udp", "port": "59602", "ip": "127.0.0.1", "type": "host" }]
            },
            "video": {
                "payload-types": [{ "id": 96, "name": "H264", "clockrate": 90000 }]
            }
        })
        .to_string();
        let link = pres.accept(&server_params).expect("str0m accepts presentation answer");
        assert_eq!(link.ssrc, expected_ssrc);

        // Data-channel video constraints map Large -> onStage + 720p, Small -> 360p, skip paused (#467).
        let sources = vec![
            TelegramGroupVideoSource {
                user_id: "u7002".into(),
                kind: PictureKind::Share,
                endpoint: "pres_1".into(),
                ssrcs: vec![3001],
                paused: false,
            },
            TelegramGroupVideoSource {
                user_id: "u7003".into(),
                kind: PictureKind::Camera,
                endpoint: "cam_2".into(),
                ssrcs: vec![3002],
                paused: false,
            },
            TelegramGroupVideoSource {
                user_id: "u7012".into(),
                kind: PictureKind::Camera,
                endpoint: "cam_3".into(),
                ssrcs: vec![3003],
                paused: true,
            },
        ];
        let watch = vec![
            WatchSource { user_id: "u7002".into(), kind: PictureKind::Share, size: WatchSize::Large },
            WatchSource { user_id: "u7003".into(), kind: PictureKind::Camera, size: WatchSize::Small },
            WatchSource { user_id: "u7012".into(), kind: PictureKind::Camera, size: WatchSize::Small },
        ];
        let json_str = build_video_constraints_json(&sources, &watch);
        let val: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(val["colibriClass"].as_str(), Some("ReceiverVideoConstraints"));
        assert_eq!(val["onStageEndpoints"].as_array().unwrap().len(), 1);
        assert_eq!(val["onStageEndpoints"][0].as_str(), Some("pres_1"));
        assert_eq!(val["constraints"]["pres_1"]["maxHeight"].as_i64(), Some(720));
        assert_eq!(val["constraints"]["cam_2"]["maxHeight"].as_i64(), Some(360));
        assert!(val["constraints"].get("cam_3").is_none(), "paused stream must not be requested");

        // RTMP segment parser handles both TGAV framed payloads and fMP4 boxes safely (#454).
        let mut tgav = b"TGAV".to_vec();
        tgav.push(1);
        tgav.extend_from_slice(&3u32.to_be_bytes());
        tgav.extend_from_slice(&[0xf8, 0xff, 0xfe]);
        tgav.push(2);
        tgav.extend_from_slice(&6u32.to_be_bytes());
        tgav.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        let parsed_tgav = parse_rtmp_segment(&tgav);
        assert_eq!(parsed_tgav.opus_frames.len(), 1);
        assert_eq!(parsed_tgav.h264_units.len(), 1);
        assert!(is_h264_keyframe(&parsed_tgav.h264_units[0]));

        let mut fmp4 = Vec::new();
        fmp4.extend_from_slice(&12u32.to_be_bytes());
        fmp4.extend_from_slice(b"moof");
        fmp4.extend_from_slice(&[0, 0, 0, 0]);
        let mdat_payload = [0u8, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f];
        fmp4.extend_from_slice(&((8 + mdat_payload.len()) as u32).to_be_bytes());
        fmp4.extend_from_slice(b"mdat");
        fmp4.extend_from_slice(&mdat_payload);
        let parsed_mp4 = parse_rtmp_segment(&fmp4);
        assert_eq!(parsed_mp4.box_types, vec![*b"moof", *b"mdat"]);
        assert_eq!(parsed_mp4.h264_units.len(), 1);

        // Truncated/garbage input never panics.
        assert!(parse_rtmp_segment(&[0, 0, 0, 2, b'b', b'a', b'd']).h264_units.is_empty());
    }

    #[test]
    fn direct_h264_camera_and_screencast_chunks_round_trip_independently() {
        let key = [0x5Au8; 256];
        let tag = [0x77u8; 16];
        // Synthetic multi-chunk H.264 IDR keyframe for camera (3500 bytes) + screencast (2600 bytes).
        let mut cam_au = vec![0x00, 0x00, 0x00, 0x01, 0x65]; // NAL type 5 = IDR
        cam_au.extend((0..3495).map(|i| (i % 251) as u8));
        let mut share_au = vec![0x00, 0x00, 0x00, 0x01, 0x67]; // NAL type 7 = SPS
        share_au.extend((0..2595).map(|i| ((i * 7) % 251) as u8));

        let cam_pkts = pack_video_chunks(&key, true, Some(&tag), DIRECT_KIND_VIDEO_CAMERA, 11, 9000, is_h264_keyframe(&cam_au), &cam_au);
        let share_pkts = pack_video_chunks(&key, true, Some(&tag), DIRECT_KIND_VIDEO_SHARE, 12, 9000, is_h264_keyframe(&share_au), &share_au);
        assert!(cam_pkts.len() >= 4);
        assert!(share_pkts.len() >= 3);

        let mut cam_reasm = VideoReassembler::default();
        let mut share_reasm = VideoReassembler::default();
        let mut got_cam = None;
        let mut got_share = None;
        for pkt in cam_pkts.iter().chain(share_pkts.iter()) {
            let (kind, _, _, payload) = unpack_direct_frame(&key, true, Some(&tag), pkt).expect("open");
            match kind {
                DIRECT_KIND_VIDEO_CAMERA => {
                    if let Some(done) = cam_reasm.push(&payload) {
                        got_cam = Some(done);
                    }
                }
                DIRECT_KIND_VIDEO_SHARE => {
                    if let Some(done) = share_reasm.push(&payload) {
                        got_share = Some(done);
                    }
                }
                other => panic!("unexpected kind {other}"),
            }
        }
        assert_eq!(got_cam, Some((cam_au, true)));
        assert_eq!(got_share, Some((share_au, true)));
    }

    #[test]
    fn peer_signal_parses_active_inactive_paused_low_battery_and_screencast() {
        let sig = build_media_state_signal(true, true, true, true);
        let parsed = parse_peer_signal(&sig).expect("parsed");
        assert_eq!(
            parsed,
            PeerMediaState { camera: true, camera_paused: true, low_battery: true, sharing: true }
        );

        let tgcalls_style = json!({
            "@type": "mediaState",
            "videoState": 2,
            "screencastState": 0,
            "isBatteryLow": false
        });
        assert_eq!(
            parse_peer_signal(&tgcalls_style),
            Some(PeerMediaState { camera: true, camera_paused: false, low_battery: false, sharing: false })
        );

        let inactive = json!({ "type": "media-state", "camera": "inactive", "screencast": "active", "low_battery": true });
        assert_eq!(
            parse_peer_signal(&inactive),
            Some(PeerMediaState { camera: false, camera_paused: false, low_battery: true, sharing: true })
        );
    }

    #[test]
    fn mix_share_audio_adds_and_clamps_stereo_frames() {
        let mut mic = [0i16; FRAME_SAMPLES];
        let mut share = [0i16; FRAME_SAMPLES];
        mic[0] = 10_000;
        share[0] = 5_000;
        mic[1] = 30_000;
        share[1] = 10_000;
        mic[2] = -30_000;
        share[2] = -10_000;
        mix_share_audio(&mut mic, &share);
        assert_eq!(mic[0], 15_000);
        assert_eq!(mic[1], i16::MAX);
        assert_eq!(mic[2], i16::MIN);
    }
}
