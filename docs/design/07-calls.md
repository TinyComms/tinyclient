# 07 — Calls

Mockups: `mockups/07-calls.html` (frame numbers below refer to it). Requirements: `../product/06-calls.md`.

**One idea.** Every call state on every network lives in one place: the **call dock** at the bottom of the sidebar. Ringing out, ringing in, connecting, in a call and failed are five faces of the same 300 px card. A voice call is that card and nothing else; the 212 px "call surface" card in today's shell goes away. Video is one surface at the top of the conversation pane, and only when asked for.

**Why the dock is at the bottom of the sidebar, not the top of the conversation.** The conversation pane changes on every navigation and belongs to whatever is open; the sidebar does not. A call in Lounge has nothing to do with the Slack channel I am reading, so it should not sit in that channel's header. The dock is also directly under the voice-channel tree it came from, and it never pushes messages down. Cost: the conversation list loses 112 px while a call exists, and only then.

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
in_call ──Tiny server lost our media──▶ reconnecting ──fresh offer, same room──▶ in_call   (3 times in 2 min, then failed)
in_call ──ring arrives──▶ in_call + incoming strip ──Accept──▶ confirm ──▶ leave, then connecting
failed ──Try again──▶ ringing_out / connecting      ──Dismiss / any new call──▶ idle
```

`in_call` is shown only when chatd reports the network confirmed the join. `mediad` exists from `connecting` (after Accept, never during a ring) until `idle`. One call at a time, across all networks.

## 1. Starting a call (frame 1)

One pill in the conversation header, 30 px high, radius 15, 12 px side padding, 14 px glyph, 12.5 px medium text, 7 px gap. The pill's look is the capability; its label never lies.

| Capability | Look | Label | Tooltip |
|---|---|---|---|
| Native, DM or group | `raised` fill, `phone` | **Call** | "Call Mara on Discord" |
| Native, voice channel | `raised` fill, `volume` | **Join voice** | "Join Lounge. People there will see you arrive." |
| Native, a call is already running | `accent_soft` fill, `accent` text | **Join call · 3** | "Iris, Jun and 1 more are in this call" |
| This conversation's call is mine | `danger_soft`, `phone-off` | **Leave** (ringing/connecting: **Cancel**) | "Leave the call  ⌘⇧H" |
| Hand-off | no fill, 1 px `hairline_strong` border, `text_soft`; the **network's own icon** 14 px, then label, then `arrow-up-right` 12 px `text_faint` | **Open in Teams** · **Open huddle in Slack** · **Open in Telegram** · **Open in Discord** | "Teams calls open in the Teams app. tinyclient doesn't carry them." / "Huddles open in Slack. Press the headphones there to join." |
| None (a Discord text channel) | no button | | |

The hand-off reads as deliberate because it is a different object: outlined, branded, with an out-arrow and a destination in its name. It never says "Call". If the official app is missing, a 32 px line appears under the header: "Teams isn't installed. **Open in browser ↗**". There is no separate video or share button in the header: camera and share are turned on inside a call. Pressing Call while in another call asks in the dock first ("Leave Lounge and call Mara Okafor?"). A voice channel joins from its header pill, its hover **Join** pill, its row menu's **Join voice**, or a double-click; a single click only opens its chat, because joining is seen by everyone there.

## 2. The call dock and bar (frames 2–5, 7)

Dock: full sidebar width (316), 8 px padding, 1 px `hairline` top border, present only when a call state exists. Card: 300 wide, radius 12, `raised` fill, 1 px `hairline` border, 10 px padding, 8 px row gap. **Height 96** (dock 112). Optional 32 px strips stack on top inside the card.

- **Row 1, 36 px, the title row.** 28 px avatar (person: round, in a 4 px speaking-ring slot; voice channel: 28 px square, radius 8, `positive_soft` fill, `volume` glyph in `positive`). Title 13 px semibold. Sub-line 11.5 px `text_soft`: network icon 11 px + context + state. Right side: the peer's `mic-off` glyph (DM), or an avatar stack (group, channel): up to three 20 px avatars overlapping 6 px, then "+N". Click the row: open that conversation ("Go to Lounge"). Click the stack: participant list.
- **Row 2, 32 px, controls.** Icon buttons 40 × 32, radius 8, 4 px gap, 16 px glyphs, left to right: mute, deafen, share, camera, devices. A flexible gap, then Leave, 48 × 32, at the right edge. Controls a stage or a network cannot do are absent, not greyed: stage 2 shows mute, deafen, devices, Leave.

Sub-line wording: `Ringing… 0:12` · `Connecting…` · `Discord · 12:04` (DM) · `tinyclient-dev · 4:31` (channel: server name) · `Reconnecting… 0:08` (`warning`) · `Call ended · 12:04`. The sub-line's tooltip in a Discord call: "End-to-end encrypted by Discord". Left alone in a DM call: `Mara left · 12:40`; tinyclient does not hang up for me.

| Control | Rest | On | Tooltip / shortcut |
|---|---|---|---|
| Mute | `raised`, `mic` in `text_soft`; while I speak, a 2 px inset `positive` ring | `danger_soft` fill, `mic-off` in `danger` | "Mute  ⌘⇧M" / "Unmute  ⌘⇧M" |
| Deafen | `raised`, `headphones` | `danger_soft`, `headphone-off` in `danger`; mute shows on too; undeafen restores the earlier mute | "Deafen  ⌘⇧D" |
| Share | `raised`, `monitor-up` | `accent_soft`, `accent` glyph | "Share your screen" / "Stop sharing" |
| Camera | `raised`, `video` | `accent_soft`, `accent` glyph | "Turn camera on" / "Turn camera off" |
| Devices | `raised`, `settings` | menu open: `selected` fill, `text` glyph | "Microphone and speaker" |
| Leave | `danger_soft`, `phone-off` in `danger`; hover: `danger` fill, `on_accent` glyph | | "Leave  ⌘⇧H" |

Hover on any rest state: `selected` fill, `text` glyph. Disabled: 40 % opacity, no hover; used while reconnecting (all but Leave) and for a moderator's mute ("Muted by a moderator in Lounge"). Ringing and connecting show only mute and devices (both may be preset) and a text **Cancel** button (32 high, `danger_soft`).

**Shortcuts** work in any tinyclient window: ⌘⇧M mute, ⌘⇧D deafen, ⌘⇧H leave or cancel, ⌘⇧A accept, ⌘⇧R decline, ⌘⇧J jump to the call's conversation. Mute can also be system-wide: off by default, a Settings switch ("Mute works while tinyclient is in the background"), registered only while a call exists. Layout and navigation arbitrates any clash with these.

**On a Tiny server** the dock stays, and the user panel sits under it (`10-tiny-server.md` §1): mic, headphones and the voice settings cog, and the connection line that opens ping, jitter, loss, bitrate and path. The channel's own stage carries a second set of controls, a floating pill of round buttons with Leave, under the people in the call. Volume and Mute for me are per person, in the person menu.

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

- **DM:** no list. The bar's avatar carries the peer's speaking ring; a `mic-off` after the name if they are muted.
- **Voice channel, in the tree.** Channel row at the tree's row height (30 px assumed): `volume` 15 px, name 13.5 px, count at the right ("5", or "2/4" with a limit). The joined channel: glyph `positive`, name `text` semibold. Under it, one row per person: **28 px**, 38 px left indent, 20 px avatar in a 4 px ring slot, 10 px gap, name 12.5 px `text_soft`, then 13 px status glyphs at the right, 5 px apart: `mic-off`, `headphone-off` (`text_faint`), `monitor-up` (`accent`; clicking it is Watch), `video`. Speaking: the ring turns `positive` and the name turns `text`. Order: friends first, then by name, the joined channel no exception; it never reorders on speech. At most 12 rows per channel, then "+ 9 more", which opens the participant list. Rows exist only for channels with people in them.
- **Honest limit:** Discord reports speaking only on the voice connection I joined. Other channels show who is there and their mute, deafen, share and camera state, never a ring.
- **Participant list** (popover from the avatar stack; the roster for group calls and whenever the tree is not on screen): 280 wide, 32 px rows, 22 px avatars, the same glyphs and ring, header "LOUNGE · 21 PEOPLE", ten rows visible then it scrolls as a virtual list.

## 6. The video surface (frame 6)

- **Opt-in.** A share or camera never opens by itself. A 32 px strip on top of the bar says "Dev Patel is sharing their screen" with a **Watch** pill (22 px high); two shares: "2 people are sharing" · **Choose…**; cameras: "Mara Okafor's camera is on" · **Watch**. Watch starts the one decode.
- **Where.** Top of the conversation pane under the header, in whatever conversation is open: it belongs to the call. Margins 20 px side, 16 px top. Card: radius 12, black fill, 1 px `hairline`, 6 px bezel, the picture aspect-fit inside, then a **32 px caption strip**: 18 px avatar, "**Dev Patel's screen** · Lounge", and at the right Pop out (`picture-in-picture-2`) and Stop watching (`x`), 28 × 24 each. No control is drawn over the picture.
- **Size.** Default: 16:9 at pane width, at most 45 % of pane height, at least 160 px. The bottom edge is a 6 px drag zone; the height is remembered. **Pop out** moves the same surface to its own window (still one decode); the pane keeps a 36 px line, "Watching in its own window · **Bring back**". Full screen is that window's green button (⌃⌘F). Double-click the picture: pop out.
- **Precedence.** The surface shows what I last chose. A watched share ending closes the surface, or falls back to cameras if I was watching cameras before. While watching cameras it follows the active speaker, switching at most once every 2 s. A share starting never replaces what I am watching; the strip offers it. Choosing a second share replaces the first.
- **My share.** Share opens the macOS system picker (screen or one window). While sharing: the button is on, and an `accent_soft` strip tops the bar: "You're sharing Zed — mod.rs" · **Stop**. macOS shows its own indicator. I do not see a preview of my own share. Permission missing: nothing starts and the strip reads, in `warning`, "tinyclient can't record the screen. Allow it in System Settings › Privacy & Security › Screen & System Audio Recording." · **Open System Settings**. Camera on is the button state alone; no self-view.

## 7. Device menu (frame 7)

Popover above the devices button, 300 wide, radius 12, 6 px padding, `popover` fill, 1 px border, one shadow. MICROPHONE: radio rows 30 px (14 px radio, `accent` when chosen), "System default *MacBook Pro Microphone*" first. Then a level meter: twelve 4 px segments, 2 px gaps, `positive` when lit, labelled "Input level"; it stays live while muted so the mic can be tested, with "Muted: others can't hear you" as the label. SPEAKER: the same rows, then **Play test sound** and a link, **Call settings…** (defaults live in Settings). A device unplugged mid-call: the call continues on the system default and the sub-line reads "Now using MacBook Pro Microphone" for 4 s.

## 7a. Voice controls (TeamSpeak and Mumble's bar)

What a call's sound does, all in mediad and the same for a Discord and a Tiny call except where
said. Settings › Voice (09 §4) holds the choices; the dock and the participant list show them.

- **Input mode.** Voice activation opens the microphone at the V6 threshold, measured after
  processing, and holds 260 ms through pauses. Push to talk opens it while the key is held and for
  the release delay after (0 to 1 s, 200 ms default), then shuts at once. A Tiny room that requires
  push to talk, or where voice activation is not allowed, is push to talk whatever the setting. The
  priority key is push to talk that also asks the server for priority speech; it opens the
  microphone in either mode. Our own speaking ring is lit exactly while we transmit.
- **Keys.** Push to talk, priority push to talk, toggle mute and toggle deafen work in any app
  (`gui/hotkeys.rs`): the shell polls the key state every 10 ms on its own thread, with no hook into
  other apps and no administrator. Windows: any key or the middle and side mouse buttons. macOS:
  any key or mouse button, once Input Monitoring is allowed; until then Settings says so. Linux
  X11: any key and the middle button; the side buttons are not visible to apps there. Wayland: no
  app may watch another's keys, so the mute and deafen keys work while tinyclient's window is in
  front and push to talk needs an X11 session; Settings says so. Mute and deafen keys do what ⌘⇧M
  and ⌘⇧D do.
- **Processing.** Echo cancellation (WebRTC's AEC3), noise suppression and automatic gain (AGC2)
  on the microphone, each a switch (V10 to V12), all on by default. The canceller hears what the
  speaker plays, cues included. This retires the headphones advice: on speakers, others no longer
  hear themselves.
- **Per-person volume.** 0 to 200 % for one person, or muted for me alone, kept per account
  (`CallGain`, V9) and applied at once. A person muted for me still lights up when they speak. For
  the participant list's row menu: a slider 0 to 200 % with a tick at 100, and "Mute for me".
- **Priority speech.** While someone holding priority (`Participant.priority`) speaks, everyone
  else is played 12 dB quieter for every listener.
- **Bitrate.** A Tiny call encodes at its room's bitrate.
- **Connection.** `CallStats` answers with the last second: round trip, jitter, loss (per mille),
  send and receive kb/s, the path (UDP, TCP, relay) and the codec. For a connection panel in the
  dock, asked about once a second while it is open; refused in words before media flows. It also
  carries the call's audio health so far: device glitches the system reported, microphone audio
  dropped or missing (overruns, gaps), the speaker running dry (underruns), and lost frames from
  others made up (concealed) or rebuilt from the next packet (FEC). A clean call keeps the first
  four at 0.
- **Audio path.** Device callbacks only move samples through lock-free rings (MMCSS "Pro Audio" on
  Windows); processing and codecs run on the call's 20 ms tick, which holds the 1 ms system timer.
  Each side resamples a hair off nominal to follow its device's clock, so nothing is spliced. A
  device glitch is counted, never a reason to reopen the device. A lost packet is rebuilt from the
  next one's FEC copy or made up from the last pitch period, faded, and crossfaded back. The mix
  goes through a look-ahead limiter, never a clamp.
- **Cues.** As 09 §8, plus Moved (a moderator moved us) and Disconnected (kicked, banned, timed out,
  or the room went), which plays out as the call ends.
- **Reconnect.** A Tiny server that stops hearing our media while its live socket stays up takes
  us back to the same room with a fresh offer, silently but for the Lost and Back cues, three times
  in two minutes before the call fails in words. A server restart already did; a Discord call
  resumes its voice session.

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

- **What repaints.** The duration and ringing counters: one text element, once a second, stopped while the window is hidden. A speaking ring: one avatar's border color, on a flip only; mediad sends edges with a 250 ms hold so it cannot flicker, about 4 repaints a second per talker at worst. The level meter: 10 Hz, only while the device menu is open. The video surface: at the stream's frame rate, in its own element. Nothing else in a call animates; idle stays idle.
- The ring slot (2 px gap + 2 px border) is always laid out with a transparent border, so a ring switching on moves nothing. Tree and participant list are uniform lists of fixed-height rows.
- The dock is a flex-none child at the end of the sidebar column; the list above it shrinks.
- The 6 px bezel exists because a video surface may not clip to rounded corners. If it can, drop it.
- Strips, the incoming card and failures are the same card with different children: one component.
- The share picker and both permission prompts are the system's; tinyclient draws none of them. Whether the system picker avoids the blanket Screen Recording permission is unverified.
- Notification action buttons show only when the user's style for tinyclient is Alerts; under Banners they sit behind "Options". First run should say so. Unverified on current macOS.

## Deliberately absent

A gallery or grid; a self-view; any control over the picture; a ringtone or call sounds of our own; a pulsing ring, waveform or animated meter; a full-window call screen; a card for voice-only calls; greyed-out buttons for things not built; a second simultaneous call; soundboard, recording, captions, reactions; a menu-bar item; a custom screen picker with thumbnails; connection-quality bars (the numbers, in words, are `CallStats`); auto-rejoin after a failure, as opposed to a blip (§7a); auto-opening a share; any in-call state before the network confirms the join.
