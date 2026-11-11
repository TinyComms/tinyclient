//! The call process: one call, from "connecting" until chatd closes our stdin.
//!
//! A Discord call: `voice` holds the pieces that need no network or device and are tested on
//! their own (`rtp`, `jitter`, `opus`, `dave`); `gateway` speaks the voice websocket; `media` is
//! the 20 ms loop that moves audio between the devices in `audio` and the UDP socket; `session`
//! ties them to the frames chatd sends.
//!
//! A Tiny server call: `tiny` is WebRTC to the server's SFU through str0m, in a 20 ms loop of its
//! own built from the same jitter buffer, codecs, gate, mixer and devices; `framing` is its
//! ICE-TCP, and `video` the threads that keep its pictures off that loop. chatd carries the offer
//! and answer over the server's live socket. `e2ee` is its
//! end-to-end encryption: the MLS group, through `davey` as for DAVE, and the frame transform
//! between the codec and str0m.
//!
//! `mediad --sound` plays one notification sound (`alert`) and exits.
//!
//! `mediad --play` is the other mode: one voice message or audio file (`play`, reading it with
//! `decode` and speeding it up with `stretch`), the speaker only, until playing stops.
//!
//! `mediad --rec <path>` is the newest: one voice note recorded off the microphone (`rec`),
//! the microphone only, until its stop, and left as an Ogg Opus file as the network's own.

pub mod alert;
pub mod audio;
pub mod broadcast;
pub mod cue;
pub mod dave;
pub mod decode;
pub mod discord_video;
pub mod e2ee;
pub mod framing;
pub mod gateway;
#[cfg(test)]
mod harness;
pub mod h264;
pub mod hear;
pub mod inproc;
pub mod jitter;
pub mod media;
pub mod ogg;
pub mod opus;
pub mod pace;
pub mod picture;
pub mod platform;
pub mod out;
pub mod permission;
pub mod pick;
pub mod play;
pub mod process;
pub mod probe;
pub mod rec;
pub mod ring;
pub mod rt;
pub mod rtp;
pub mod serve;
pub mod session;
pub mod stats;
pub mod stretch;
pub mod talk;
pub mod tiny;
pub mod video;
pub mod video_stats;

/// Samples a second, on the wire and in every buffer between the codec and the devices.
pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one 20 ms frame.
pub const FRAME: usize = 960;
/// Interleaved samples in one frame.
pub const FRAME_SAMPLES: usize = FRAME * CHANNELS;

/// chatd's Verbose logging changed while this mediad runs (`MediaRequest::LogLevel`): the file
/// takes the new ceiling from the next line. An unknown word leaves it as it was.
pub fn relevel(level: &str) {
    if let Some(guard) = protocol::log_policy::current() {
        guard.apply(guard.policy().overlay_stored(Some(level), None, None));
    }
}
