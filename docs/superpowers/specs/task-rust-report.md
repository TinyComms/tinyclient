# Task report: fixture devices and call controls

Status: DONE_WITH_CONCERNS

Commit: `c6af2be726fd5a4c7cab367116063ef9a11ac390` on `feat/call-e2e` (not pushed).

## What landed

`TINYCLIENT_CALL_FIXTURE` is a directory. When it is set and `TINYCLIENT_MEDIAD_AUDIO` is not, mediad lists one input whose id and name are `fixture` and that input is the default. `Audio::capture` reads `speech.pcm` (48 kHz stereo little-endian `i16`) one 20 ms frame per call and loops. No microphone is opened. `probe_camera` and `probe_screen` succeed. `Pictures::camera` returns the next Annex B access unit of `camera-h.h264` and `camera-l.h264`, high then low, looping. `Pictures::share` returns the next access unit of `share.h264`. `TINYCLIENT_MEDIAD_AUDIO` still wins for listing and capture when both are set. Played audio follows `TINYCLIENT_MEDIAD_SINK`, or goes nowhere, so a speaker device is not opened either. `Audio::simulated` is true for the fixture, so a call does not ask for microphone permission.

`button` and `button_with_icon` set `Role::Button` and the accessible name from the label, so Join (`tiny-join`, `tiny-join-empty`) is "Join channel". `bar_button` takes that name and sets the role. Mute, deafen, share, camera, devices, health, and leave use the tooltip words already chosen in `tiny_call_bar`. `button_disabled` still has no role, no element id, and no click.

## Fixtures

Committed under `crates/mediad/tests/fixtures/call/`:

- `speech.pcm` — System.Speech sentence "Tiny sent a spoken sentence across the call." ffmpeg is not installed. The harness in `crates/mediad/tests/call_fixture.rs` decodes the 22.05 kHz mono wav (`tests/fixtures/speech.wav`) to 48 kHz stereo `s16le` by linear resampling. RMS of the committed clip is about 3196.
- `camera-h.h264` and `camera-l.h264` — copied from `tinyserver/crates/sfu/fixtures/`.
- `share.h264` — one IDR, 640×360, four quadrants (red, green, blue, white), encoded with this crate's OpenH264 baseline encoder (`Coded::open` / `encode`, screen content, intra forced). 888 bytes.

A unit test decodes the first camera-high frame and the share frame and requires their average RGB distance to be greater than 20.

## Tests

Watched failing first:

- Listing with the variable set panicked with `no fixture input`.
- Capture opened the real microphone (`Microphone (Lyra-CAM-Audio_36HW)`) instead of `fixture`.
- The sink file was not written.
- Camera frames were a live encode, not the clip.
- The share colour test panicked because `share.h264` was missing.
- Call-control tests failed to compile: `bar_button` took three arguments, the tooltip-word helpers did not exist, and a disabled button has no element id getter under that name.

Then green:

- `cargo test -p mediad --test call_fixture` — 8 passed.
- `cargo test -p mediad --lib` — 143 passed, 4 ignored (real devices and timing). Includes `the_share_clip_is_not_the_camera_bars`.
- `cargo test -p mediad --test tiny --test play` — 11 + 4 passed. These set `TINYCLIENT_MEDIAD_AUDIO`.
- `cargo test -p tinyclient --features gui` filters for the new button and channel tests, plus `tiny::channel::tests` — passed.

## Concerns

- `cargo test -p mediad --test voice` (the Discord-path `TINYCLIENT_MEDIAD_AUDIO` suite) failed before any assertion: `openssl` is not on PATH, so the suite cannot mint its TLS certificate. That failure is environmental and happens in `voice.rs` setup. `tiny.rs` and `play.rs`, which also set `TINYCLIENT_MEDIAD_AUDIO`, passed.
- The colour test loads Cisco's OpenH264 DLL from `%LOCALAPPDATA%\tinyclient`. `ureq` in this crate is built with `default-features = false` and `native-tls`, and `ureq::get` then reports that no TLS backend is configured, so the crate's own download does not fetch the DLL. The DLL was placed with Python for this run. A machine without that cache fails the colour test at the same download.
- Fixture mode does not open a speaker. Playback goes to `TINYCLIENT_MEDIAD_SINK` or is dropped, the same stand-in the tone path uses. The spec names the microphone, camera, and screen.
