# 07 — Calls

Mockups: `mockups/07-calls.html` (frame numbers below refer to it). Requirements: `../product/06-calls.md`.

**One idea.** Every call state on every network lives in one place: the **call dock** at the bottom of the sidebar. Ringing out, ringing in, connecting, in a call and failed are five faces of the same 300 px card. The dock carries call *state* wherever you navigate; the **call view** is where a call is *looked at*: a Tiny room opens its stage in the main pane, while a joined direct call or Discord call shows its picture area above the conversation (and a compact preview tile above the dock while another chat is open), and any live video can pop out into its own always-on-top window. Discord voice is joined from the server list.

**Why the dock is at the bottom of the sidebar, not the top of the conversation.** The conversation pane changes on every navigation and belongs to whatever is open; the sidebar does not. A call in Lounge has nothing to do with the Slack channel I am reading, so it should not sit in that channel's header. The dock is also directly under the voice-channel tree it came from, and it never pushes messages down. Cost: the conversation list loses 112 px while a call exists, and only then.

**Why the call view is separate from the dock.** Keeping state in the dock lets you read any other chat while staying in control of mute, deafen and Leave. When you look at the call itself — a Tiny room stage or a direct/Discord call's picture — the main pane gives it room for participant tiles, a focused share, a self-view PiP, full screen, and pop-out without cramping the sidebar.

## States

```
idle ──Call (DM, group)──────────▶ ringing_out ──answered──▶ connecting ──joined──▶ in_call
idle ──Join voice (channel)──────────────────────────────▶ connecting
idle ──ring arrives──▶ ringing_in ──Accept──▶ connecting
ringing_out ──Cancel──▶ idle            ──no answer / declined──▶ failed
ringing_in  ──Decline / caller stops / answered elsewhere──▶ idle (+ row in the conversation)
connecting  ──Cancel──▶ idle            ──refused / 10 s without a reply / mic denied──▶ failed
in_call ──Leave──▶ idle   ──other side ends──▶ ended (4 s) ──▶ idle
in_call ──packets stop──▶ reconnecting ──back──▶ in_call   ──30 s──▶ failed ("Connection lost")
in_call ──ring arrives──▶ in_call + incoming strip ──Accept──▶ confirm ──▶ leave, then connecting
failed ──Try again──▶ ringing_out / connecting      ──Dismiss / any new call──▶ idle
```

`in_call` is shown only when chatd reports the network confirmed the join. `mediad` exists from `connecting` (after Accept, never during a ring) until `idle`. One call at a time, across all networks. The call view exists from `ringing_out` / `connecting` through `idle`: in a Tiny room, the stage shows the connecting/reconnecting/failure banner over the tiles; in a direct or Discord call, the conversation shows its live call state and reveals the picture area once joined with video.

## 1. Starting a call (frame 1)

One pill in the conversation header, 30 px high, radius 15, 12 px side padding, 14 px glyph, 12.5 px medium text, 7 px gap. The pill's look is the capability; its label never lies.

| Capability | Look | Label | Tooltip |
|---|---|---|---|
| Native, DM or group | `raised` fill, `phone` | **Call** | "Call Mara on Discord" |
| Native, voice channel | `raised` fill, `volume` | **Join voice** | "Join Lounge. People there will see you arrive." |
| Native, a call is already running | `accent_soft` fill, `accent` text | **Join call · 3** | "Iris, Jun and 1 more are in this call" |
| This conversation's call is mine | `danger_soft`, `phone-off` | **Leave** (ringing/connecting: **Cancel**) | "Leave the call  ⌘⇧H" |
| Hand-off | no fill, 1 px `hairline_strong` border, `text_soft`; the **network's own icon** 14 px, then label, then `arrow-up-right` 12 px `text_faint` | **Open in Teams** · **Open huddle in Slack** · **Open in Telegram** · **Open in Discord** · **Open in Messages** | "Teams calls open in the Teams app. tinyclient doesn't carry them." / "Huddles open in Slack. Press the headphones there to join." |
| None (a Discord text channel, or Signal, where calls are not built) | no button | | |

The hand-off reads as deliberate because it is a different object: outlined, branded, with an out-arrow and a destination in its name. It never says "Call". If the official app is missing, a 32 px line appears under the header: "Teams isn't installed. **Open in browser ↗**". There is no separate video or share button in the header: camera and share are turned on inside a call. Pressing Call while in another call asks in the dock first ("Leave Lounge and call Mara Okafor?"). A voice channel joins from its header pill, its hover **Join** pill, or a double-click; a single click only opens its chat, because joining is seen by everyone there. Discord voice is joined from the server list. Camera and share turn on inside the call, not on this pill.

## 2. The call dock and bar (frames 2–5, 7)

Dock: full sidebar width (316), 8 px padding, 1 px `hairline` top border, present only when a call state exists. Card: 300 wide, radius 12, `raised` fill, 1 px `hairline` border, 10 px padding, 8 px row gap. **Height 96** (dock 112). Optional 32 px strips stack on top inside the card.

- **Row 1, 36 px, the title row.** 28 px avatar (person: round, in a 4 px speaking-ring slot; voice channel: 28 px square, radius 8, `positive_soft` fill, `volume` glyph in `positive`). Title 13 px semibold. Sub-line 11.5 px `text_soft`: network icon 11 px + context + state. Right side: the peer's `mic-off` glyph (DM), or an avatar stack (group, channel): up to three 20 px avatars overlapping 6 px, then "+N". Click the card or title row: open the call (the Tiny room stage for a Tiny room, or the call's conversation for a direct or Discord call). Click the stack: participant list.
- **Row 2, the controls.** Joined controls share one row equally and wrap as a set: mute, deafen, share, camera, devices, Leave. Share and camera are included on a Tiny room that allows video, on a Tiny direct call, and on a joined Discord call. On a Tiny server's own column, the dock collapses to the compact bar (signal bars, security slot, Leave) and share/camera live on the room's call bar. Each button grows to the same width. The place name wraps on the line under the title. The elapsed clock sits with the signal bars and the security slot, not beside the name. Controls a stage or a network cannot do are absent, not greyed: stage 2 shows mute, deafen, devices, Leave.

Sub-line wording: `Ringing… 0:12` · `Connecting…` · `Discord · 12:04` (DM) · `tinyclient-dev · 4:31` (channel: server name) · `Reconnecting…` (`warning`) · `Call ended · 12:04`. A Tiny call does not count down time left. The sub-line's tooltip in a Discord call: "End-to-end encrypted by Discord". Discord DAVE seals the voice, and the picture when one is on. Left alone in a DM call: `Mara left · 12:40`; tinyclient does not hang up for me.

| Control | Rest | On | Tooltip / shortcut |
|---|---|---|---|
| Mute | `raised`, `mic` in `text_soft`; while I speak, a 2 px inset `positive` ring | `danger_soft` fill, `mic-off` in `danger` | "Mute  ⌘⇧M" / "Unmute  ⌘⇧M" |
| Deafen | `raised`, `headphones` | `danger_soft`, `headphone-off` in `danger`; mute shows on too; undeafen restores the earlier mute | "Deafen  ⌘⇧D" |
| Share | `raised`, `monitor-up` | `accent_soft`, `accent` glyph | "Share your screen" / "Stop sharing" |
| Camera | `raised`, `video` | `accent_soft`, `accent` glyph | "Turn camera on" / "Turn camera off" |
| Devices | `raised`, `settings` | menu open: `selected` fill, `text` glyph | "Devices" |
| Leave | `danger_soft`, `phone-off` in `danger`; hover: `danger` fill, `on_accent` glyph | | "Leave  ⌘⇧H" |

Hover on any rest state: `selected` fill, `text` glyph. Disabled: 40 % opacity, no hover; used while reconnecting (all but Leave) and for a moderator's mute ("Muted by a moderator in Lounge"). Ringing and connecting show only mute and devices (both may be preset) and a text **Cancel** button (32 high, `danger_soft`).

**Shortcuts** work in any tinyclient window: ⌘⇧M mute, ⌘⇧D deafen, ⌘⇧H leave or cancel, ⌘⇧A accept, ⌘⇧R decline, ⌘⇧J open the call (jumps to the Tiny room or the call's conversation). Mute can also be system-wide: off by default, a Settings switch ("Mute works while tinyclient is in the background"), registered only while a call exists. Layout and navigation arbitrates any clash with these.

**On a Tiny server** the user panel sits under the dock (`10-tiny-server.md` §1). Over that server's column the dock is the compact bar: signal bars, `Voice connected`, the place (`Lounge / Night Shift`), the security slot, and Leave. Anywhere else the same call is the full card.

### 2a. You're muted

While muted in a call that is up, a Tiny stage shows a `danger_fill` pill over the call bar, "You're muted · ⌘⇧M to talk" (`10-tiny-server.md`). The dock says the same on its sub-line.

## 3. Outgoing and failed (frames 2, 8)

Ringing shows who, an elapsed counter and Cancel, in the dock; the header pill becomes Cancel and the conversation's row preview reads "Ringing…" in `positive`. Ringing ends when the network says so; with no word from it, after 60 s as "No answer". A failure replaces the card (auto height: 28 px `warning_soft` disc with a glyph, 13 px semibold title, wrapped 11.5 px reason, a button row, a 20 px dismiss ×) and stays until dismissed or the next call.

| Title | Reason line | Buttons |
|---|---|---|
| No answer | "Mara Okafor didn't pick up." | Call again · Message |
| Declined | "Mara Okafor declined the call." | Message |
| Couldn't connect | "Discord refused the voice connection." / "No reply from Discord after 10 seconds." / "The call engine stopped." | Try again · Open in Discord |
| tinyclient can't use the microphone | "Allow it in System Settings › Privacy & Security › Microphone, then call again." | Open System Settings |
| No microphone found | "Plug one in, or choose another in Microphone and speaker." | Try again |
| Lounge is full | "10 of 10 people are in it." · or "You don't have permission to join Lounge." | (dismiss only) |
| Discord needs a newer tinyclient for calls | "Discord raised its encryption requirement. Update tinyclient, or take this call in Discord." | Open in Discord |
| Connection lost | "No reply from Discord for 30 seconds. You've left Lounge." | Rejoin · Open in Discord |

## 4. Incoming (frame 3)

- **In the window:** the dock card with a 1 px `positive` border at 45 %. Title: the caller. Sub-line "Incoming call" (group: "Iris Navarro is calling"). Row 2 is two equal buttons: **Decline** (`danger_soft`, `phone-off`) and **Accept** (`positive` fill, `on_accent` text, `phone`). It never takes keyboard focus, opens no conversation and plays no sound of its own; the caller's row preview reads "Incoming call". Static: no pulse.
- **Already in a call:** a 40 px strip on top of the bar: 20 px avatar, "**Iris Navarro** is calling · Weekend crew", two 32 × 26 buttons. Accept asks in the card: "Leave Lounge and answer Iris Navarro?" / "You can be in one call at a time." / **Stay in Lounge** · **Leave & answer**.
- **System notification** (posted by the daemon, so it arrives with the window closed). Title: the caller, or the group's name. Subtitle: "Discord · Incoming call". Body: "Voice call" / "Iris Navarro is calling." / in a call: "…You're in Lounge; accepting leaves it." Actions: **Accept**, **Decline**. Clicking the body opens the window on that conversation without answering. Accept opens the window already `connecting`. The system plays the sound and applies Focus.
- **It stops:** the notification is withdrawn and the card goes. Caller gave up: a "Missed call" row, unread, and an ordinary "Missed call from Mara Okafor" notification. Answered on another device: a quiet row, no unread. Declined: a quiet row.

## 5. Participants (frames 5, 7)

- **DM:** no participant list popover. The dock avatar carries the peer's speaking ring (with a `mic-off` after the name if they are muted), and while video is active the call view shows the peer's picture with a self-view corner PiP (or a compact preview tile above the dock when another conversation is open).
- **Voice channel, in the tree.** Channel row at the tree's row height (30 px assumed): `volume` 15 px, name 13.5 px, count at the right ("5", or "2/4" with a limit). The joined channel: glyph `positive`, name `text` semibold. Under it, one row per person: **28 px**, 38 px left indent, 20 px avatar in a 4 px ring slot, 10 px gap, name 12.5 px `text_soft`, then 13 px status glyphs at the right, 5 px apart: `mic-off`, `headphone-off` (`text_faint`), `monitor-up` (`accent`; tooltip "Sharing their screen" — watching happens in the room stage or dock), `video`. Speaking: the ring turns `positive` and the name turns `text`. Order: me first, then by name; it never reorders on speech. At most 12 rows per channel, then "+ 9 more", which opens the participant list. Rows exist only for channels with people in them.
- **Honest limit:** Discord reports speaking only on the voice connection I joined. Other channels show who is there and their mute, deafen, share and camera state, never a ring.
- **Participant list** (popover from the avatar stack; the roster for group calls and whenever the tree is not on screen): 280 wide, 32 px rows, 22 px avatars, the same glyphs and ring, header "LOUNGE · 21 PEOPLE", ten rows visible then a plain scrolling column.

## 6. The call view and pictures (frames 6, 10)

**Video is a Tiny room, a Tiny direct call, or a Discord call.** Share and Camera appear on the dock (when outside the Tiny server's compact column) and on a Tiny room's call bar while a Tiny room that allows video, a Tiny direct call, or a Discord call is joined. Any other joined call leaves them off. They are absent, not greyed.

- **Tiny voice channel stage:** the stage holds a wrapping grid of participant tiles (`10-tiny-server.md` §2) plus stream tiles for anyone sharing their screen or camera. Clicking a tile focuses it; clicking a stream watches it and fills the stage, with participant tiles in a strip below, overlaid, or hidden, and a full-screen toggle (Esc leaves full screen or returns to the grid). The floating call bar sits at the bottom of the stage.
- **Direct calls and Discord calls:** when the call's conversation is open, a picture box sits above the message list showing the watched peer's screen or camera (with a `LIVE` badge for shares, Stop watching / Stop controls, and a 112 × 64 corner self-view PiP when sending your own camera or screen). When another conversation is open, a compact 126 px picture tile appears above the dock card so you can keep watching without leaving your chat.
- **Capture and codec:** the call sends and receives one H.264 picture (the share, otherwise the active speaker's camera). The camera is the system's camera on Windows, macOS, and Linux, and the phone's camera on Android and iOS; camera device selection lives in Settings › Audio & video (row V22). Screen share is a screen or a window on Windows, the primary screen on macOS, the X11 root on an X11 Linux session, and the desktop portal's picker on Wayland. Android uses MediaProjection. iOS uses a broadcast extension for the whole screen, and the simulator shares this app.

### Getting to and from the call view

- Clicking the dock card (or pressing ⌘⇧J) jumps to the Tiny room stage for a Tiny room, or opens the call's conversation for a direct or Discord call.
- Back and forward navigation history records both conversations and Tiny channels, so returning to what you were reading takes one click or shortcut.
- Pressing Esc exits full-screen stream mode first, then unfocuses a focused tile back to the grid.

### When a share starts and auto-watch

- **1:1 Tiny direct calls:** auto-watch starts decoding as soon as the peer turns on screen share or camera, and stops when they turn it off. Choosing **Stop watching** is respected until the peer stops and starts again.
- **Tiny rooms:** Watch remains opt-in — a stream tile appears on the stage ("Watch stream") and clicking it begins decoding and focuses it.
- **Discord calls:** active-speaker camera selection debounces switches by 800 ms (`MediaRequest::Speaker`), and Go Live shares are watched on demand via the Watch control.

When the server offers `e2ee_calls`, the client seals Opus and the H.264 picture and leaves the one-byte NAL header clear. Discord DAVE does not use that lock.

## 6a. The picture on a Discord call

A camera is one H.264 layer on the voice connection the call already has. Go Live is a second connection that sends the screen and does not open a microphone. Watching someone else's share is a third connection that only receives. The voice connection stays up. The same Share and Camera buttons turn these on. On desktop, received Discord camera and Go Live frames draw in the call picture area above the conversation (or the compact dock tile when away), one decoded picture at a time. A stage still opens in Discord and has no share or camera here.

DAVE seals the picture as well as the voice. The client offers H.264 and does not offer another video codec. There is no simulcast. The camera and the screen are the same captures a Tiny room uses.

## 7. Device menu (frame 7)

Popover above the devices button, 300 wide, radius 12, 6 px padding, `popover` fill, 1 px border, one shadow. MICROPHONE: radio rows 30 px (14 px radio, `accent` when chosen), "System default *MacBook Pro Microphone*" first. Then a level meter: twelve 4 px segments, 2 px gaps, `positive` when lit, labelled "Input level"; it stays live while muted so the mic can be tested, with "Muted: others can't hear you" as the label. SPEAKER: the same rows, then **Play test sound** and a link, **Call settings…** (defaults live in Settings). A device unplugged mid-call: the call continues on the system default and the sub-line reads "Now using MacBook Pro Microphone" for 4 s.

## 7a. Processing and reconnect

Echo cancellation, noise suppression, and automatic gain are switches and default on. While echo cancellation is off, a Tiny channel still says to wear headphones.

A Tiny call that loses its media resumes three times in two minutes, pausing 1 s, then 2 s, then 3 s. The sub-line and the channel banner say `Reconnecting…`. They do not count down time left. The link grade on the signal bars is `positive`, `warning` at Fair, and `danger` at Poor (`10-tiny-server.md`).

## 7b. What the call seals

When the server offers `e2ee_calls`, the client seals Opus and the H.264 picture and leaves the one-byte NAL header clear. Discord DAVE seals Opus, and on a picture the H.264 access unit. The security slot is the call's one indicator of that protection (`gui/call/security.rs`); a call that says nothing about protection draws none. The Discord tooltip stays "End-to-end encrypted by Discord". The Tiny lock is not drawn on a Discord call.

## 8. Ending and what remains (frames 4, 8, 9)

Leave removes the bar at once. The other side ending shows "Call ended · 12:04" with Dismiss for 4 s. Each DM or group call leaves one **system row** in the conversation: 36 px, a 30 px disc with a 14 px glyph in the avatar column, 13 px `text_soft` text, the clock time, an optional 24 px pill.

| Row | Glyph | Pill |
|---|---|---|
| "**Missed call** from Mara Okafor" (disc `danger_soft`; counts as unread) | `phone-missed` | Call back |
| "Call with Mara Okafor · 12 min 4 s" | `phone` | |
| "You called Mara Okafor · no answer" | `phone-outgoing` | Call again |
| "You declined a call from Mara Okafor" | `phone-off` | |
| "Call from Mara Okafor · answered on another device" | `phone-incoming` | |
| "**Call in progress** · Iris Navarro, Jun Aoki and 1 more · 4:10" (disc `accent_soft`) | `phone` | Join |

Voice channels leave no rows. A failed call leaves its card in the dock, not a row.

## New tokens

`positive_soft` rgba(0x3fcf8e24) · `warning_soft` rgba(0xe3a94a24) · `hairline_strong` rgba(0xffffff1f) (hand-off outline, popover border) · `popover` rgb(0x1c1d25), opaque, because `raised` is translucent and cannot sit over content · `video` rgb(0x000000) · `shadow_popover` 0 12 32 rgba(0,0,0,.5). Speaking uses `positive`; no new color. New glyphs (Lucide): `mic`, `mic-off`, `headphones`, `headphone-off`, `video`, `settings`, `volume-2`, `arrow-up-right`, `x`, `picture-in-picture-2`, `phone-missed`, `phone-incoming`, `phone-outgoing`, `triangle-alert`. **Rule change asked of the visual system:** "red is only for leaving a call" becomes "red is for leaving a call, my own mic or audio being off, and a missed call".

## GPUI notes

- **What repaints.** The duration and ringing counters: one text element, once a second, stopped while the window is hidden. A speaking ring: one avatar's border color, on a flip only; mediad sends edges with a 250 ms hold so it cannot flicker, about 4 repaints a second per talker at worst. The level meter: 10 Hz, only while the device menu is open. The one H.264 picture: at the stream's frame rate, in its own element in the call view (Tiny room stage, direct/Discord conversation picture box, or compact dock preview). Nothing else in a call animates; idle stays idle.
- The ring slot (2 px gap + 2 px border) is always laid out with a transparent border, so a ring switching on moves nothing. The participant list is a plain scrolling column of fixed-height rows.
- The dock is a flex-none child at the end of the sidebar column; the list above it shrinks.
- The one received picture is drawn in the Tiny room stage (`10-tiny-server.md`) or, for direct and Discord calls, in the conversation's call picture box (and a compact preview above the dock while another chat is open).
- Strips, the incoming card and failures are the same card with different children: one component.
- On desktop, screen share uses tinyclient's own share picker (`gui/tiny/share.rs`) with screen/window thumbnails, quality selection (`Best`, `Balanced`, `Low`), and a pointer toggle; Wayland uses the desktop portal picker, and microphone/camera/screen permission prompts are the OS's own.
- Notification action buttons show only when the user's style for tinyclient is Alerts; under Banners they sit behind "Options". First run should say so. Unverified on current macOS.

## Deliberately absent

A multi-stream video gallery (only one remote H.264 picture is decoded at a time); a pulsing ring, waveform or animated meter; greyed-out buttons for things not built; a second simultaneous call; soundboard, recording, captions, call reactions; a menu-bar call item; auto-rejoin after a failure (a Tiny blip resumes three times in two minutes, §7a); any in-call state before the network confirms the join.
