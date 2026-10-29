//! The call process: one call, from "connecting" until chatd closes our stdin.
//!
//! A Discord call: `voice` holds the pieces that need no network or device and are tested on
//! their own (`rtp`, `jitter`, `opus`, `dave`); `gateway` speaks the voice websocket; `media` is
//! the 20 ms loop that moves audio between the devices in `audio` and the UDP socket; `session`
//! ties them to the frames chatd sends.
//!
//! A Tiny server call: `tiny` is WebRTC to the server's SFU through str0m, in a 20 ms loop of its
//! own built from the same jitter buffer, codecs, gate, mixer and devices; `framing` is its
//! ICE-TCP. chatd carries the offer and answer over the server's live socket. `e2ee` is its
//! end-to-end encryption: the MLS group, through `davey` as for DAVE, and the frame transform
//! between the codec and str0m.
//!
//! `mediad --play` is the other mode: one voice message or audio file (`play`, reading it with
//! `decode` and speeding it up with `stretch`), the speaker only, until playing stops.

pub mod audio;
pub mod cue;
pub mod dave;
pub mod decode;
pub mod e2ee;
pub mod framing;
pub mod gateway;
#[cfg(test)]
mod harness;
pub mod hear;
pub mod jitter;
pub mod media;
pub mod opus;
pub mod out;
pub mod permission;
pub mod pick;
pub mod play;
pub mod process;
pub mod probe;
pub mod ring;
pub mod rt;
pub mod rtp;
pub mod session;
pub mod stats;
pub mod stretch;
pub mod talk;
pub mod tiny;

/// Samples a second, on the wire and in every buffer between the codec and the devices.
pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one 20 ms frame.
pub const FRAME: usize = 960;
/// Interleaved samples in one frame.
pub const FRAME_SAMPLES: usize = FRAME * CHANNELS;
