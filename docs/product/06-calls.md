# 06 — Calls

Voice, video and screen share on Discord, Slack, Telegram and Teams, and room calls with sound on a Tiny server you run. Researched 2026-10-13. Updated 2026-10-27 for Tiny room calls.
Fixed and not reopened here: media engines never load into chatd; mediad exists only during a
call; one video surface (active speaker or one share); one hardware H.264 encode for a share.

## The rule that fixes "I call and that's not wired up"

The shell shows an in-call state **only after the network confirms we joined**. Until a network's
calls are built, its call button is a hand-off and says so ("Open huddle in Slack"). Pressing Call
never again produces a card with nobody on the other end.

## Feasibility, with evidence

| Network | Verdict | What a user-session client must do today | Evidence |
| --- | --- | --- | --- |
| Discord voice | **Feasible, pure Rust. Account risk.** | Voice gateway v8, UDP, Opus 48 kHz stereo, transport AEAD (`aead_aes256_gcm_rtpsize` preferred, `aead_xchacha20_poly1305_rtpsize` required), **and DAVE** (MLS, voice opcodes 21–31, `max_dave_protocol_version` in identify). | DAVE is mandatory since 2 Mar 2026 for DMs, group DMs, voice channels and Go Live; a non-DAVE client is rejected with close code 4017 [1][2][3]. Non-DAVE clients cannot join anything any more. The Rust crate `davey` (OpenMLS) implements DAVE and is what songbird uses [4]. |
| Discord video / Go Live | **Hard.** | A second voice connection for the stream, H.264 RTP, DAVE frame encryption on video. | Discord-video-stream (user accounts) closed its DAVE issue Dec 2025 [5]; that it still works after enforcement is *unverified*. Stream signalling for user accounts is undocumented. |
| Slack huddle | **Hard, plausible. Spike first.** | Slack session → Chime meeting credentials (JoinToken) → Chime signalling (protobuf over WebSocket) → one WebRTC PeerConnection, Opus, H.264. | Huddles are built on the Amazon Chime SDK [6]. Slack has no API to join one [7]. One project (huddlefm PR #149) joins the Chime meeting natively without a browser, using protobufs from amazon-chime-sdk-js 3.31 — but states it was **not tested against a live huddle** [8]. Which Slack call returns the credentials is *unverified*. |
| Telegram 1:1 | **Feasible and sanctioned. Heavy.** | TDLib does signalling only; media is tgcalls on tg_owt (Telegram's WebRTC fork), a large C++ build. | A TDLib client added 1:1 voice+video this way and interoperates with official clients; build time doubled, ~30k symbols, software VP8/VP9 only [9]. Hardware H.264 through tgcalls on macOS is *unverified*. |
| Telegram group | **Hard.** | tgcalls group instance; new encrypted conference calls also need the tde2e chain and per-packet encryption. | [10]. The client in [9] shipped no group calls. |
| Teams | **Hard, not impossible. Hand-off for now.** | Trouter signalling + SRTP media against Microsoft's relays, on a work tenant. | Correction to our earlier assumption: a non-Microsoft client *has* interoperated. `eisbaw/ost` (Rust) lists audio calls as working and video as WIP [11]; Better Teams (SwiftUI over that core) claims 1:1 audio/video and meeting join [12]. Both are README claims, *not verified by us*. Microsoft supports no third-party media endpoint [13]. |
| Tiny server room | **Shipped, audio only.** | We run the server, so nothing is reverse engineered: str0m in mediad (`mediad/src/tiny.rs`), one send-only Opus m-line plus one receive slot per other seat, a UDP host candidate and an active ICE-TCP candidate (RFC 6544 [14]); UDP preferred, TCP where UDP is blocked. | [15]. No echo cancellation yet; wear headphones (the README and a mediad comment say so, not the GUI). |

Dependencies outside calls: Telegram and Teams have no adapter at all yet, so their calls cannot
come before their text. Discord and Slack text exist but are not yet run against real accounts.

## Priority

| | Discord | Slack | Telegram | Teams | Tiny server |
| --- | --- | --- | --- | --- | --- |
| Voice in/out, mute, deafen, devices, leave, roster, speaking | **P0** | P1 after spike | P2 (1:1) | hand-off | **shipped, audio only** |
| Ring out / ring in (DM, group DM) | **P0** | P1 (huddle invite) | P2 | hand-off | n/a (rooms are joined, not rung) |
| Watch one share | P1 | P1 | P2 | hand-off | future |
| Send one share (one hardware H.264 encode) | P1 | P2 | P2 | hand-off | future |
| Camera, send and active-speaker receive | P2 | P2 | P2 | hand-off | future |
| Group calls beyond the above | n/a | n/a | hand-off | hand-off | n/a |
| Gallery, recording, captions, noise suppression UI, soundboard, stage channels, activities, lobby, breakout rooms | cut | cut | cut | cut | cut |

## Stages

1. **Honest button (days).** Every call button is a hand-off labelled with the official app; no
   mediad, no in-call card. Incoming rings are shown and notified (signalling only, in chatd).
2. **Discord voice.** Join a server voice channel; call a DM and be
   called. Chosen because the adapter exists and the whole path is Rust with no WebRTC stack:
   gateway voice state, voice gateway v8, UDP, Opus, `davey`. Small mediad.
3. **Discord watch one share, then send one.** One H.264 decode; then one VideoToolbox encode.
4. **Slack huddle audio.** Gated on a two-day spike that joins a real huddle and is heard. This
   puts a WebRTC stack in mediad; pick one that Telegram can share or accept two.
5. **Telegram 1:1** after the Telegram text adapter, with tgcalls loaded only once a call starts.
6. **Teams** stays hand-off until the Teams adapter exists; then re-evaluate against [11].
7. **Tiny server room calls — shipped.** Join a room's call with sound where the server has audio
   turned on: str0m in mediad, one send-only Opus m-line plus receive slots, UDP preferred and
   ICE-TCP where UDP is blocked. Mute, deafen, speaking rings, join and leave cues. Screen share
   and a camera there are future.

## Stories

Each check is observable from outside the process. "Peer" is a second account on the official app.

**Join and talk**
1. I join a Discord voice channel and the people in it hear me, and I hear them.
   *Check:* peer in the official app sees me in the channel, hears a tone I play, and I record theirs.
2. I call a Discord DM and their app rings; when they answer we talk.
   *Check:* peer's official app rings within 3 s; `status` goes `ringing` → `connected` only after they accept.
3. I see who is in the call and who is speaking.
   *Check:* `status` lists the participants the official app lists; `speaking` flips within 500 ms of the peer talking.
4. I mute, and I deafen.
   *Check:* peer's app shows my mute and deafen icons; while muted mediad sends no audio packets; while deafened it plays none.
5. I pick my microphone and speaker, and a headset unplugged mid-call does not drop the call.
   *Check:* `status` names the devices in use; pulling the device moves to the system default within 2 s, call still `connected`.
6. I leave, and it is over everywhere.
   *Check:* peer sees me leave within 2 s; no mediad in the process table within 2 s; mic indicator in the menu bar is off.

**Being called**
7. Someone calls me and I see who, on which network, with Accept and Decline.
   *Check:* a macOS notification and an incoming row in `status` within 2 s of the peer pressing call; no mediad yet.
8. I accept and I'm talking; I decline and it stops ringing for both of us.
   *Check:* accept → one mediad and audio both ways; decline → peer's app stops ringing, no mediad was ever spawned.
9. I'm called with the window closed.
   *Check:* shell not running, chatd running: the notification still arrives; Accept opens the shell already in the call.
10. I'm called while in a call.
    *Check:* a notification, no ringtone, no second mediad; accepting asks first, then leaves the first call.
11. A call I missed is still there afterwards.
    *Check:* the conversation shows "Missed call" with the time, and it counts as unread.

**One share, one camera**
12. I watch one share in a call.
    *Check:* mediad reports exactly one share decode; asking for a second replaces the first; frames match the peer's screen.
13. I share my screen or one window.
    *Check:* peer sees it; mediad reports one encode and it is a hardware session; stopping removes it and the screen-recording indicator.
14. I turn my camera on, and I see the active speaker's camera when nobody is sharing.
    *Check:* peer sees my video; at most one speaker decode exists and it follows `speaking`.

**When tinyclient can't**
15. On a network or call type tinyclient does not carry, the button says so and takes me there.
    *Check:* label reads "Open in Teams" (never "Call"); pressing it opens the official app on that call or meeting; no mediad; `status` never reports a call.
16. If a call fails I'm told why, in words, and offered the official app.
    *Check:* killing the network mid-join yields a named reason ("Discord refused the voice connection") within 10 s and no leftover mediad.

**Cost**
17. Calls do not cost me the chat process.
    *Check:* chatd RSS after ten join/leave cycles is within 1 MB of before; no media, codec or WebRTC symbols in the chatd binary (`nm`); chatd answers `ping` in under 50 ms throughout a call.
18. A network's call engine is loaded only for that network's call.
    *Check:* mediad in a Discord voice call maps no WebRTC or tgcalls library (`vmmap`).

## Incoming calls while the shell is closed

chatd already holds each network's socket, and a ring is signalling, not media: Discord's call
event on the gateway, Slack's huddle invite on its socket, TDLib's call update (exact event names
*unverified*). chatd records the ring, raises `version`, and posts a notification with Accept and
Decline. The OS plays the notification sound, so no audio code enters chatd. mediad starts on
Accept and not before. A ring nobody answers becomes a missed-call row. Do Not Disturb / Focus is
respected by using the system notification path rather than our own sound.

## What the wire must be able to say (for whoever builds it)

Today `Call` carries only network, conversation and video streams. Stories 1–16 need: a call state
(ringing out, ringing in, connecting, connected, failed + reason), participants with speaking and
mute flags, self mute/deafen, input/output device, an incoming-call record, accept/decline, and a
per-conversation capability (native call / hand-off / none) so the button can label itself.

## Hand-off behavior

The button names the destination. One click opens the official app as close to the call as its
links allow: Teams meeting links and `msteams:` call links land on the join screen; `slack://`
opens the channel (the user presses the headphones); `discord://` opens the channel; `tg://` opens
the chat. These link forms are from each vendor's deep-link scheme and *not re-tested here*. If the
official app is not installed, the row says "Teams isn't installed" and offers the web link.

## Risks the owner should hear plainly

- **Discord:** voice from a user account on an unofficial client is the most visible thing a
  third-party client does, and DAVE identity makes each device distinguishable. Ban risk is real
  and not measurable from outside.
- **Slack:** the whole path is undocumented and may be closed by a server change; on a work
  workspace this is the feature most likely to draw an admin's attention.
- **DAVE moves:** Discord has raised the floor once and can again; `davey` must be kept current
  or Discord calls stop on a date Discord picks.
- **Size:** Slack and Telegram each bring a WebRTC-class engine. That lands in mediad, never in
  chatd, but it is tens of MB on disk and real RSS during a call.

- **Echo:** mediad cancels no echo, on Discord calls or Tiny server calls. Discord's own clients
  cancel it on their side; tinyclient relies on nothing but the speech gate, which keeps the
  speaker out only while it is quieter than speech. With open speakers the others hear
  themselves. Until there is echo cancellation, the README and a mediad comment note to wear headphones. The next
  step, measured against the 40 MB mediad budget before it is taken: a pure-Rust port of
  WebRTC's AEC3 fed the mix as the far end (`sonora-aec3` or `aec3` on crates.io, both young), or
  the system's voice processing (macOS VoiceProcessingIO, Windows communications mode), which
  cpal does not open today.

## Open questions for the owner

1. Is the Discord account you use day to day one you accept losing? If not, Discord calls stay
   hand-off and the first native call becomes Telegram.
2. Is your Slack a work workspace whose admin would object? That decides whether the huddle spike
   happens at all.
3. Does Teams stay hand-off for good, or do we re-evaluate once the Teams text adapter exists,
   given a Rust client now claims working audio?
4. First shippable call: Discord voice channel join, or Discord DM ring-out/ring-in? (Recommended:
   channel join first, DM ringing the week after.)
5. May chatd post notifications with Accept/Decline while the window is closed, which requires it
   to live inside the app bundle?

## Sources

1. Discord, "Every Voice and Video Call on Discord Is Now End-to-End Encrypted" — https://discord.com/blog/every-voice-and-video-call-on-discord-is-now-end-to-end-encrypted
2. Discord support, minimum client version for voice (close code 4017) — https://support.discord.com/hc/en-us/articles/38025123604631-Minimum-Client-Version-Requirements-for-Voice-Chat
3. Discord developer docs, Voice Connections (gateway v8, modes, DAVE opcodes) — https://docs.discord.com/developers/topics/voice-connections
4. `davey`, DAVE in Rust on OpenMLS — https://github.com/Snazzah/davey
5. Discord-video-stream DAVE issue #102 — https://github.com/Discord-RE/Discord-video-stream/issues/102
6. AWS, Chime SDK customers (Slack huddles) — https://aws.amazon.com/chime/chime-sdk/customers/
7. OpenClaw Slack huddles plugin (joins through the web client; no API) — https://docs.openclaw.ai/plugins/slack-huddles
8. huddlefm PR #149, native Chime join without Chromium — https://github.com/ingoau/huddlefm/pull/149
9. harbour-fernschreiber PR #583, TDLib + tgcalls 1:1 calls — https://github.com/Wunderfitz/harbour-fernschreiber/pull/583
10. Telegram, E2E group calls — https://core.telegram.org/api/end-to-end/group-calls
11. `eisbaw/ost`, Teams protocol in Rust — https://github.com/eisbaw/ost
12. Better Teams — https://github.com/mrowlinson/better-teams
13. Microsoft, Teams call flows — https://learn.microsoft.com/en-us/MicrosoftTeams/microsoft-teams-online-call-flows
14. RFC 6544, TCP Candidates with Interactive Connectivity Establishment — https://www.rfc-editor.org/rfc/rfc6544
15. PR #71, "Join a Tiny room with sound, over UDP or ICE-TCP" — https://github.com/itz4blitz/tinyclient/pull/71
