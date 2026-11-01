# 09. Settings and sound

The window on main (0.2.35) has twelve sections: Accounts, General, Notifications, Sounds, Appearance, Messages, Privacy, Audio & video, Voice, Storage, Shortcuts, About. About is pinned to the bottom. Search sits on the section list. A missing control is absent, not greyed out. Search for a known gap returns the line in `NOT_YET`.

**A control exists only when it changes behavior.**

The tables under "Information architecture" are the control design. A row marked "in" is in the window on main. A row marked "not in 0.2.35" is absent. The window size 720 × 560 was not re-checked against the layout constants.

## Shipped rows

| Section | Rows | Notes |
| --- | --- | --- |
| General | G2, G4 | G2 "Keep receiving when the window is closed" is in the window on macOS only. Linux uses the systemd user unit when that unit is enabled; quitting the GUI then leaves chatd up either way. G4 is automatic updates from the public GitHub release. |
| Notifications | N1, N2, ND, NG, NM, NT, N4, NL, N5–N11, N13 | ND / NG / NM / NT are "Direct messages", "Group chats", "Mentions in channels", "Tiny servers". There is no N3 per-account menu. NL is the lock-screen preview and is hidden on macOS, where the OS does that. N10 and N11 use the host's flash and badge labels and are hidden on Linux. |
| Sounds | S1–S4, SV | Five message sounds, two rings, or None. No "System alert sound". SV is notification volume and is hidden on macOS. |
| Appearance | A2, A3, A4 | Text size, density, list previews. No theme control. The app is dark. |
| Messages | M1–M5 | Send key, where a conversation opens, typing line, emoticons, GIF and sticker autoplay. |
| Privacy | P1–P4, PE | Typing and read receipts per account, image loading, keep-for 7 / 30 / 90 days. Telegram typing and receipts are fixed on. Signal says the phone. Messages says the phone. |
| Audio & video | V1, V2, V4, V5 | Microphone, speaker, join muted, call volume. |
| Voice | V3, V6, V8, V10–V19, S7–S10 | Input level, sensitivity, voice activation or push-to-talk, echo cancellation, noise suppression, automatic gain, key recorders, release delay, call cues and their volume. |
| Storage | D1–D3 | Usage, cap (default 500 MB), clear. Sign-ins, drafts, and settings stay. |
| Shortcuts | read-only | Menu shortcuts. Voice keys are recorders in Voice, not this list. |
| About | B1–B3 | Diagnostics, logs, reset. The tag line should name all seven networks. The string in the window still names five. |

chatd keeps the notification, sound, privacy, voice, and storage values. The shell keeps theme-adjacent choices in `shell.prefs` (there is no theme value; the section word is stored). Unknown lines in `shell.prefs` are kept. `Request::Settings`, `Request::Set`, and `Change::Settings` are on main.

## Not in the window

| Gap | What search says, or what the code does |
| --- | --- |
| G1 Open at login | "tinyclient has no open-at-login setting yet." |
| G3 Menu bar | No tray icon on this commit's search line. |
| A1 Light theme | "tinyclient is dark only for now." |
| N3 per-account notify menu | Not a row. ND, NG, NM, and NT are global. chatd can store an account-level notify word; the window does not show N3. |
| N14, N15 | No reaction or thread-follow notify control. |
| S5, S6 pickers | chatd will honor an account or conversation sound if one is stored. No row was found that writes them. |
| S13 custom files, S14 ring-back | Not offered. |
| M6 spellcheck, P5 screen-share hide, P6 Touch ID, P7 Signal relay | In `NOT_YET` or absent. |
| V7 camera device | "tinyclient has no camera yet." A Tiny call can still toggle the camera from the call dock. That is `06-calls.md`. |
| V9 per-person volume | `CallGain` on a Tiny participant row, not a Settings row. |
| Proxy, accent, font, language, sync | `NOT_YET`. |

## Never

Accent colours, a font picker, per-network theme colours, an editor for ordinary menu shortcuts, in-app banner style, an in-app Reduce Motion, a hardware-acceleration switch, an experimental tab, a language picker, a cache-location picker, and a settings sync service.

Voice processing and the push-to-talk keys are settings. They are not in this list.

Written 2026-10-14 from a survey of Slack, Discord, Telegram, Signal, Teams, Messages and Zed, and an audit of this repo. The design tables below keep that survey. "in" means the row is in the window on main. "not in 0.2.35" means it is absent.

## Goals

1. **Broad.** The window has twelve sections, covering notifications, sound, appearance, messages,
   privacy, audio, voice, storage and startup. People who switch from Discord and Slack expect to tune
   the client, and the owner wants that.
2. **Real.** Every control has a check that someone can run from outside the process.
3. **Calm by default.** Out of the box, tinyclient notifies about DMs and mentions only and
   plays one soft sound for those, a 4 s ring for a call, and short quiet cues inside a call.
   Nothing else makes a sound.
4. **Local.** Settings live on this computer. Nothing is fetched, synced, uploaded or reported.
5. **Free when closed.** Settings costs nothing while its window is closed, and sound costs
   nothing while nothing is playing.

## Non-goals

- In-app copies of OS controls: banner style, alert volume, Focus schedules, Reduce Motion,
  12/24-hour clock, language. tinyclient follows the OS and says where the OS control is.
- The networks' own account settings: password, 2FA, profile, privacy on the server,
  per-channel notification levels. These are done in the official app; tinyclient honours what
  the network reports (mutes, Do Not Disturb, `suppress_everyone`).
- Settings sync across devices, cloud backup, import from the official apps, and an online
  settings store.
- "Experimental", "hardware acceleration" and debug switches.

Echo cancellation, noise suppression, and automatic gain are V10, V11, and V12 in Voice, default on. Push-to-talk and the other voice keys are recorders in Voice. How a call uses the microphone, the echo canceller, and the camera toggle is `06-calls.md`.

## Rules this doc changes

The owner asked for sound and for breadth. These earlier rules change, on purpose:

| Rule | Where | Now |
| --- | --- | --- |
| "In-app sounds" deliberately absent | design 06 §Deliberately absent | In-call cues play in mediad on the call's own output (S7 to S10). Message sounds still go through the system notification. |
| "A ringtone or call sounds of our own" absent | design 07 §135 | One bundled ring, at most 4 s, played by the notification (S4). In-call cues as above. |
| "Custom sounds" absent | design 08 §Deliberately absent | A choice of bundled sounds is in the window. Files of your own are not offered. |
| One sound, the system default | design 06 §6 | A sound picker: five message sounds, two rings, or None. "System alert sound" is not a choice in 0.2.35. |
| Dock bounce absent | design 06 §Deliberately absent | An option, default Off (N10). |
| Media cache default 1 GB | design 08 §5 | 500 MB, matching `07-budgets-and-risk.md` §6 and `CACHE_BYTES`. |
| One switch per network | design 08 §5 | ND, NG, NM, and NT are global. N3's per-account menu is not in the window. |

## Where settings live

Each setting has exactly one home. The Settings window reads and writes both.

| Home | What goes there | Why |
| --- | --- | --- |
| chatd: table `settings(scope, key, value)` in `chat.sqlite` | Anything chatd acts on while the window may be closed: notifications, sounds, pause, quiet hours, privacy, call devices, storage, quit behaviour | chatd posts notifications and talks to the networks; the window may not be running |
| Shell: `shell.prefs` | Anything only the window draws or reads: theme, text size, density, send key, list previews, last Settings section | It is read before the first frame, so the window opens looking right |
| The OS | Notification permission, Focus. Open at login is not a setting (G1). | The OS is the source of truth; tinyclient reads it, never caches a copy |

`scope` is `""` for global, `account:<network>:<id>` for one account (one Slack workspace is one
account), and `conv:<network>:<id>` for one conversation.

`Request::Settings` returns every stored value, `Request::Set { scope, key, value: Option<String> }` stores one (`None` restores the default), and `Change::Settings` tells every window. That store is on main. Unknown lines in `shell.prefs` are kept. There is no theme value; the section word is stored.

## Information architecture

The window is 720 × 560 in this design (not re-checked against the layout constants), a section list on the left, a search field at
its top, About pinned to the bottom. Sections, in order:

Accounts · General · Notifications · Sounds · Appearance · Messages · Privacy · Audio & video ·
Voice · Storage · Shortcuts · About

Columns below: **Type** is the control. **Scope** is G (global), A (per account), C (per
conversation). **Home** is `d` (chatd) or `s` (shell). **Rel** is `in`, `not in 0.2.35`, or Later (also absent; see Later).

### Accounts

One group per network, a row per account with state in words, Sign out, Sign in again, Add. There is no N3 link from an account row to a per-account notify menu. ND, NG, NM, and NT are global.

### General

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| G1 | Open at login | Switch | | G | OS | Not in 0.2.35. Search: "tinyclient has no open-at-login setting yet." | not in 0.2.35 |
| G2 | Keep receiving when the window is closed | Switch | On | G | d | On: closing the window hides it and chatd keeps running. Off: closing the window quits the window and chatd. In the window on macOS only. Linux uses the systemd user unit when that unit is enabled. | in |
| G3 | Show in menu bar | Switch | Off | G | s | | Later |
| G4 | Install updates automatically | Switch | On | G | s | On: a newer GitHub release is downloaded while the window is open and replaces this copy on quit. Off: the notice still appears and nothing is downloaded until asked | in |

Caption under G2: "Closing the window leaves tinyclient running. Quit with ⌘Q to
stop it." ⌘Q stops chatd and the window unless the optional Linux systemd user
service is enabled; in that mode systemd owns the daemon's lifetime (see below).

### Notifications

The top line, only when the OS has denied permission: "Notifications are turned off for
tinyclient in System Settings." with "Open System Settings".

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| N1 | Show notifications | Switch | On | G | d | Off: chatd posts no message notifications. Counts and badge keep working. Calls follow N5 | in |
| N2 | Notify me about | ND, NG, NM, NT: "Direct messages", "Group chats", "Mentions in channels", "Tiny servers" | DMs, groups, and mentions | G | d | Which arriving messages chatd posts. Muted conversations never notify (04 C2). These are global, not per account. | in |
| N3 | Per account: Notify me about | Menu | | A | d | Not a row. chatd can store an account-level notify word; the window does not show N3. | not in 0.2.35 |
| N4 | Show in notifications | Segmented: "Sender and message" / "Sender only" / "Nothing" | Sender only | G | d | Sender only: title is the sender, body "New message" or "New mention". Nothing: title is the network, body "New message". For screen sharing | in |
| NL | Lock-screen preview | Switch | | G | d | Hidden on macOS, where the OS does that. | in |
| N5 | Incoming calls | Segmented: "Ring" / "Notify silently" / "Don't notify" | Ring | G | d | Ring posts the call notification with the ring sound (S4). Silently posts it with no sound. Don't notify posts nothing; the in-window card and the missed-call row still appear | in |
| N6 | During a call | Segmented: "Banners only" / "Banners and sounds" / "Nothing" | Banners only | G | d | What message notifications do while I am in a call. Rings follow 07 §4 regardless | in |
| N7 | Pause notifications | Button with menu: "For 1 hour" / "Until tomorrow" (08:00 local) / "Until I resume"; becomes "Resume" while paused | Not paused | G | d | Nothing is posted until the stored end time; counts and badge keep counting. Also in the rail bell and the tinyclient menu | in |
| N8 | Quiet hours | Switch, then From and To time fields | Off; 22:00 to 08:00 | G | d | Inside the window nothing is posted except calls when N9 is on | in |
| N9 | Let calls ring during quiet hours and pauses | Switch | On | G | d | On: a call still posts during N7 and N8 | in |
| N10 | Bounce the Dock icon | Segmented: "Off" / "Once" / "Until I look" | Off | G | s | The host's flash or bounce, for anything that posts a notification while the window is not key. Hidden on Linux. | in |
| N11 | Dock badge counts | Segmented: "Direct messages and mentions" / "All unread" / "Off" | DMs and mentions | G | d | The number on the Dock icon. Muted conversations never count. Hidden on Linux. | in |
| N12 | Per conversation: Notifications | In Conversation › Notifications…: "Mute" (the existing network mute) | Network's state | C | network | Unchanged from today: written to the network with `SetMute` | in |
| N13 | Send a test notification | Button | | | | Posts one real notification, "This is how messages will look and sound", using N4 and S2. Under Focus it is silenced like any other, and the caption says so | in |
| N14 | Notify about reactions to my messages | Switch | Off | G | d | | Later |
| N15 | Notify about replies in threads I follow | Switch | On | G | d | | Later |
| N16 | Focus filter (show only some accounts in a macOS Focus) | App Intents | | A | OS | | Later |

Caption under the group: "Do Not Disturb you set in Slack or Discord is followed here. For
schedules by place or app, use Focus in System Settings; tinyclient follows it."

Behaviour with no extra setting (from 04 N1 to N7 and design 06
§6): no notification for the conversation open in the key window, my own messages from any
device, muted conversations, or history at connect; one notification per conversation, replaced
in place; withdrawn when read anywhere; Reply and Mark as Read actions; the network's own Do Not
Disturb (Discord status, Slack `dnd.info`) suppresses posting for that account; Discord
`suppress_everyone` and `suppress_roles` and Slack's channel prefs decide whether `@everyone`,
`@here` and `@channel` count as mentions (04 C3).

### Sounds

The full sound design is in the next section. The controls:

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| S1 | Play sounds | Switch | On | G | d | Off: every notification posts with no sound and mediad plays no cues. The test buttons still play | in |
| S2 | Message sound | Picker: Pebble, Drop, Tap, Chime, Glass, None | "Pebble" | G | d | The sound on a message notification. No "System alert sound". | in |
| S3 | Mention sound | Picker: "Same as messages", the 5 sounds, None | Same as messages | G | d | The sound when the notification is for a mention. No "System alert sound". | in |
| S4 | Ring | Picker: 2 bundled rings, None | "Soft ring" | G | d | The sound on the incoming-call notification. No "System alert sound". | in |
| S5 | Per account: Message sound | | | A | d | chatd will honor an account sound if one is stored. No row was found that writes it. | not in 0.2.35 |
| S6 | Per conversation: Sound | | | C | d | chatd will honor a conversation sound if one is stored. No row was found in Settings or the conversation side pane that writes it. | not in 0.2.35 |
| S7 | In calls: someone joins or leaves | Switch | On | G | d | mediad plays the join or leave cue | in |
| S8 | In calls: I mute, unmute, deafen or undeafen | Switch | On | G | d | mediad plays the matching cue | in |
| S9 | In calls: connection lost or back | Switch | On | G | d | mediad plays the matching cue | in |
| S10 | Call sound volume | Slider 0 to 100 % | 50 % | G | d | Gain on the cues in S7 to S9 only. Voices are not affected | in |
| S11 | Play | A small play button beside S2, S3, S4 | | | | Plays that sound once, on the device the notification would use (below). S5 is not a row. | in |
| S12 | Play test sound | Button beside S10 | | | | mediad plays the join cue at S10's volume on the call speaker (V2) | in |
| S13 | Add a sound file… | Picker entry | | G | d | | Later (spike) |
| S14 | Ring-back while I am calling someone | Switch | | G | d | | Later |

S7–S10 live in the Voice section of the window. SV, notification volume, is hidden on macOS, where the OS owns alert volume. Caption under S2 to S4: "Notification sounds use the alert volume in System Settings › Sound." There is no "System alert sound" choice.

### Appearance

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A1 | Theme | | | G | s | Not in 0.2.35. Search: "tinyclient is dark only for now." | not in 0.2.35 |
| A2 | Text size | Five stops: −1 to +3 (body 13, 14, 15, 16, 17) | 0 (14) | G | s | Type sizes and `row_height`. Same state as ⌘+ ⌘− ⌘0, so the control never shows nothing selected | in |
| A3 | Density | Segmented: "Comfortable" / "Compact" | Comfortable | G | s | Compact: list rows 48 → 40, avatars 28 → 24, 2 pt less between message groups | in |
| A4 | Show message previews in the list | Switch | On | G | s | Off: list rows show the name and time only. For screen sharing | in |
| A5 | Accent colour, fonts, chat backgrounds, per-network colour | | | | | | Never |

Reduce Motion, Increase Contrast and Reduce Transparency are read from macOS and honoured, with
no control.

### Messages

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| M1 | Send with | Segmented: "Return" / "⌘Return" | Return | G | s | Return: Return sends, ⇧Return adds a line. ⌘Return: Return adds a line, ⌘Return sends. ⌥Return still sends silently. Bindings change at once | in |
| M2 | Open conversations at | Segmented: "First unread" / "Newest message" | First unread | G | s | Where the message list lands on open. First unread is today's behaviour (07 R3) | in |
| M3 | Show when others are typing | Switch | On | G | s | Off: no typing line is drawn. What I send is P1 | in |
| M4 | Convert typed emoticons to emoji | Switch | Off | G | s | On send, `:)` `:(` `:D` `;)` `<3` standing alone become 🙂 🙁 😀 😉 ❤️. Never inside code spans | in |
| M5 | Play GIFs and animated stickers | Segmented: "Automatically" / "On hover" / "Never" | Automatically | G | s | Automatically: every GIF, animated WebP, APNG and animated emoji on screen plays while the window is key, within the 07 budgets. On hover: a still with a GIF badge, playing under the pointer. Never: a still with a badge and a play disc; a click opens it in the viewer, which plays it. Reduce Motion makes Automatically play under the pointer. Kept in `shell.prefs` | in |
| M6 | Check spelling | Switch | | G | s | | Later |

### Privacy

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | Let others see when I am typing | Per account switch; fixed rows for Telegram, Signal, and Messages | Discord off, Slack on, Teams on. Telegram: "Always on", caption "Telegram does not let an app turn this off." Signal: "Set on your phone". Messages: the phone decides. | A | d | Off: the adapter sends no typing signal | in |
| P2 | Let others see what I have read | Teams switch; fixed rows for Telegram, Signal, and Messages | Teams on. Telegram "Always on". Signal "Set on your phone". Messages: the phone decides. | A | d | Teams off: no consumption-horizon update is sent; the chat is still marked read here. Caption: "Off also leaves chats unread on your other devices." Group caption: "Discord and Slack have no read receipts." | in |
| P3 | Load images and link previews automatically | Switch | On | G | s | Off: each image or preview is one line with its size; a click loads it. Nothing is fetched until clicked | in |
| P4 | Keep messages on this computer for | Segmented: "7 days" / "30 days" / "90 days" | 90 days | G | d | The age half of the retention rule (`store.rs`); the 2,000 per conversation cap stays. Pruned within 1 min of the change and daily | in |
| PE | Only join end-to-end encrypted calls | Switch | Off | G | s | Off on purpose: a server or an app that cannot encrypt calls would stop working. Either way a Tiny call whose server turns encryption off stops sending and playing ("Encryption was turned off" on the call badge); off, the security panel offers "Carry on without encryption", on, it offers only leaving, and a server with no end-to-end calls is not joined. Incognito hosts (`tinycloud.chat`, `incognito.tinyclient.io`) and `e2ee_required` rooms behave as on whatever the switch says. Read when a call is joined | in |
| P5 | Hide tinyclient from screen sharing and recordings | Switch | | G | s | | Later (spike: macOS 15 capture may ignore `sharingType`) |
| P6 | Lock with Touch ID | Switch | | G | s | | Later |
| P7 | Signal: always relay calls | Switch | | A | d | | Later, with Signal calls |

### Audio & video

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| V1 | Microphone | Menu: "System default (name)" then devices | System default | G | d | chatd sends it to mediad right after `Join`. Shown by name; a missing device falls back to the default and the menu says "(not connected)". Audio & video is where the default device lives. | in |
| V2 | Speaker | Menu, as V1 | System default | G | d | As V1, for output. Also where S12 plays | in |
| V4 | Join calls with my microphone muted | Switch | Off | G | d | On: every join and every Accept starts muted, and the dock shows it | in |
| V5 | Call volume | Slider 0 to 100 % | 100 % | G | d | Gain on everyone's voice in mediad's mix. | in |

### Voice

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| V3 | Input level | Test button and meter | | | | While on, the meter is live. Outside a call, mediad starts for it and exits after Stop. | in |
| V6 | Microphone sensitivity | Slider over the live meter | | G | d | The voice gate threshold in mediad. | in |
| V8 | Voice activation or push to talk | Switch and key recorder | | G | d | In the window. | in |
| V10–V19 | Processing, keys, release delay | Switches and recorders | V10–V12 on | G | d | Echo cancellation, noise suppression, automatic gain (default on), key recorders (V13 and V15–V17: push-to-talk, priority, mute, deafen), and release delay. A downloaded noise-suppression model is not V11. | in |
| S7–S10 | Call cues and their volume | Switches and a slider | Cues on | G | d | Join, leave, mute, deafen, connection, and their volume. Cue semantics during a call are `06-calls.md`. | in |
| V7 | Camera device | | | G | d | Not in the window. Search: "tinyclient has no camera yet." A Tiny call dock can toggle the camera. That toggle is `06-calls.md`. | not in 0.2.35 |
| V9 | Per-person volume | | | C | | `CallGain` on a Tiny participant row, not a Settings row. | not in 0.2.35 |

"Call settings…" opens Audio & video. A device picked in the call dock lasts for that call and resets when the call ends. How a call uses the microphone, the echo canceller, and the camera toggle is `06-calls.md`.

### Storage

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| D1 | Usage | Total, two-part bar (Messages, Images and files), "Show in Finder" | | | | Read from `Store::media_on_disk` and the database file size | in |
| D2 | Keep images and files up to | Segmented: 250 MB / 500 MB / 1 GB / 2 GB | 500 MB | G | d | `Blobs::evict`'s cap, oldest unused first; takes effect within 1 min | in |
| D3 | Clear… | Button, then confirm (08 §5 copy) | | | | Deletes `media/` and message rows, then vacuums. Sessions, drafts and settings stay | in |

### Shortcuts

A read-only list of every shortcut, grouped as the menus are, searchable. ⌘/ opens it. Caption:
"To change a shortcut, add it in System Settings › Keyboard › Keyboard Shortcuts › App
Shortcuts." Every command is in a menu, so that works for all of them. An editor for ordinary menu shortcuts is Never. Voice keys are recorders in Voice, not this list.

### About

The tag line should name Discord, Slack, Telegram, Teams, Signal, Messages, and Tiny. The string in the window still names five: "One small app for Slack, Discord, Telegram, Teams and Signal."

| # | Control | What it does |
| --- | --- | --- |
| B1 | Copy diagnostics | Version, OS, service state, and account connection states. No message text, names, tokens, or log lines. Save debug log writes the in-memory ring under `logs/bundles/` and shows that directory |
| B2 | Show logs in Finder | Opens `logs/` (`tinyclient.log`, `chatd.log`, `mediad.log`, `login.log`, and `bundles/`) |
| B3 | Reset all settings… | Confirm: "Reset every setting to its default? Accounts, messages and files are not touched." Deletes every row in `settings` with global or account scope and the settings lines in `shell.prefs`. Per-conversation sounds go too |

### Count

The 2026-10-14 count of "30 of 50" is not the window on main. What 0.2.35 shows is the "Shipped rows" table at the top of this file. Gaps are "Not in the window".

## Sound design

### Two paths, no third

| Path | Plays | How | Cost |
| --- | --- | --- | --- |
| The notification | Message, mention and ring sounds | chatd sets the notification sound to a named bundled file, or to nil for None. "System alert sound" is not a picker choice in 0.2.35. | 0 bytes of audio code, 0 memory. macOS applies Focus, the alert volume, the alert device and its per-app "Play sound for notifications" switch |
| mediad | In-call cues and the test sound | Synthesised tones mixed into `play()` on the chosen output, after deafen | About 2 KB of code, no assets. mediad runs only in a call or a test |

The window process gets one small exception: S11's preview plays the bundled file once with
`NSSound`, routed to the system alert device (`kAudioHardwarePropertyDefaultSystemOutputDevice`),
because that is where the notification would play. The object is released when it finishes. If
routing to the alert device cannot be done, S11 is left out rather than playing somewhere else.
No `rodio` or `cpal` in the shell, and no audio in chatd.

### Events

| Event | Sound | Default | Silent when |
| --- | --- | --- | --- |
| DM, or a message in scope of N2/N3 | S2, or S5/S6 | "Pebble", 0.35 s | Any rule below |
| Mention | S3, or S5/S6 | Same as messages | Any rule below |
| Incoming call | S4 | "Soft ring", two soft rises, 4 s | N5 not Ring; S1 off; paused or quiet hours with N9 off |
| Someone joins / leaves my call | Rising / falling two notes, 180 ms | On, in voice channels and group calls with at most 25 people | S7 off; I am deafened; more than 25 people (Discord channels are noisy) |
| I mute / unmute | Low / high tick, 60 ms | On | S8 off |
| I deafen / undeafen | Low / high double tick, 120 ms | On | S8 off. Undeafen plays even though I was deafened |
| Call connection lost / back | Low falling pair / rising pair, 250 ms | On | S9 off |
| Missed call, reply failed, signed out, reconnect summary | None; banner only | | Always silent |
| Anything I do: send, react, type, open, launch | None | | Never a sound |

Silence rules for notification sounds, applied by chatd in this order: S1 off; N1 off or the
account at Off; paused (N7) or quiet hours (N8); the network's own Do Not Disturb; in a call
and N6 is not "Banners and sounds"; the conversation is open in the key window (no notification
at all); the window is key on another conversation (banner without sound, design 06 §6); a
replacement within 10 s of the last sound for that conversation; another sound in the last 3 s
from any conversation. The OS then applies Focus, its own mute and the app's sound permission.

### The sounds

- Five message sounds: "Pebble", "Drop", "Tap", "Chime", "Glass". Two rings: "Soft ring",
  "Classic ring". Synthesised by a script in the repo (sine and FM with short envelopes), so the
  licence is ours and they can be rebuilt.
- Message sounds at most 0.5 s, peak −14 dBFS, no energy under 300 Hz (laptop speakers) and
  none above 6 kHz (not shrill). Rings at most 4 s, peak −10 dBFS.
- CAF, IMA4, mono, 22.05 kHz, since nothing in them is above 6 kHz: about 11.7 KB a second.
  The seven files are 114 KB (`packaging/sounds`, rendered by `packaging/sounds/render.sh`). At
  44.1 kHz the 8 s of rings alone would pass the 160 KB budget.
- The ring stops when the notification is withdrawn (caller gave up, answered elsewhere,
  declined). A 4 s cap keeps an unwithdrawable sound short. A longer looping ring is Later,
  after checking that `removeDeliveredNotifications` cuts a playing sound.
- Cues are generated, not stored: at 100 % of S10 a cue peaks at −20 dBFS, under normal speech,
  so a cue never covers a voice.

### Volume

- Notification sounds: on macOS they follow the alert volume, and SV is hidden there. SV is the notification-volume row on the platforms that show it.
- Cues: S10, because they share the call's output and must sit under voices at the user's taste.
- Voices: V5.

### Custom files (Later, spike first)

"Add a sound file…" accepts `.aiff`, `.wav` or `.caf` up to 30 s and 1 MB, checked by header
only, copied to `~/Library/Sounds/tinyclient-<hash>.<ext>` and added to every sound picker. The
spike: does `UNNotificationSound(named:)` find a file there for a non-sandboxed, signed app on
macOS 13 to 26. If it does, this is about 100 lines and 0 bytes at rest. If it does not, there is
no honest way without in-app audio, and it stays out.

### Windows and Linux

Same rules, mapped where the OS allows. Windows toasts can play only the system's
`ms-winsoundevent` sounds from an unpackaged app, so the pickers list those plus "None". Linux
uses the `sound-name` hint from the freedesktop sound theme, or "None" through `suppress-sound`.
Where a platform cannot honour a control, the control is not shown on that platform.

## Search within Settings

- A field at the top of the section list. ⌘F focuses it; typing anywhere in the list focuses it.
- Matches the label, the caption, the section name, the values of segmented controls and menus,
  and a keyword list per control ("ding", "chime", "volume", "dark mode", "do not disturb",
  "snooze", "quiet", "Enter", "receipts", "cache"). Case- and accent-insensitive, matched at word
  starts. Ranked: label, then values, then keywords, then caption.
- Results replace the pane: one row per match, "Section › Group" in 12 `text_faint` above the
  live control, so a switch can be changed in the results. Return on a result opens its section
  with that row highlighted by a `selected` fill for 1.5 s.
- No match: "No settings match "proxy"." If the term is in the Later or Never list, one honest
  line follows: "tinyclient has no proxy setting yet." The list of those terms is a constant.
- The index is a static table built at compile time. Search runs on each keystroke in under 1 ms
  for 80 entries and allocates nothing on the heap past the query string.

## Keyboard

- ⌘, opens Settings on the last section used (stored in `shell.prefs`); ⌘W and Esc close it. Esc
  in a non-empty search clears it first.
- ↑ ↓ move through sections when the list has focus; ⌘1 to ⌘9 jump to the first nine sections.
- Tab and ⇧Tab move through controls in reading order, with the focus ring of `01-system.md`.
  Space toggles a switch or presses a button. ← → change a segmented control or slider stop;
  ⌥← ⌥→ move a slider by 10. Return opens a menu; arrows choose; Return picks.
- Every control has an accessible label equal to its visible label, and a value.
- Pause notifications (N7) is also in the tinyclient menu as "Pause Notifications", so App
  Shortcuts can bind it.

## Nothing comes from servers

- No setting is read from a network's account settings and copied into tinyclient. What the
  network enforces (mutes, Do Not Disturb, `suppress_everyone`, Signal's receipt and typing
  choices, a Teams admin's policy) is honoured live from the network's events and shown as a
  fixed row or caption, never stored as a tinyclient value.
- No setting is written to a network, except the mute that already exists (N12) and is the
  network's own.
- No import from the official apps, browsers or other Macs. No export, no sync, no backup.
- Settings files hold no tokens, message text or names. Per-conversation rows hold only the ids.

## Persistence and migration

- **Only changed values are stored.** A missing row means the default, so a better default
  reaches everyone who never touched the control.
- **chatd:** `settings` gets a `version` row. On start, before serving the socket, chatd runs any
  pending migration in one transaction (rename a key, convert a value). An unknown key is kept
  and ignored; a value that does not parse is treated as the default and logged once.
- **Shell:** `shell.prefs` keeps theme-adjacent choices (`density`, `send`, `previews`,
  `settings_section`, and so on). There is no theme value; the section word is stored. Unknown lines are kept, so an older build does not erase a newer
  build's choices.
- **First run of this release:** nothing to migrate. The existing `text` line keeps its meaning.
  The stale in-call device pick is not migrated.
- **Sign-out** deletes that account's `account:` and `conv:` rows in the same transaction as its
  messages (`07-budgets-and-risk.md` §6), so no trace of a removed account stays in settings.
- **Changes apply at once.** chatd re-reads the row on `Set`, sends `Change::Settings`, and every
  window redraws in the next frame. No Save, no restart.

## Budgets

Measured as in `07-budgets-and-risk.md`: release build, stripped; `footprint` for the shell and
mediad, RSS for chatd; 60 s after the state is reached.

| What | Budget |
| --- | --- |
| Shell binary, all v1 settings, search, previews | +400 KB |
| chatd binary, settings store, notifications, sound choice | +1 MB (most of it the `UNUserNotificationCenter` bindings) |
| mediad binary, cues, test sound, gains, gate setting | +20 KB |
| Bundled sounds in the app's Resources | 160 KB |
| chatd RSS, idle, with 200 per-conversation rows stored | +0.5 MB |
| Shell footprint, idle, Settings window never opened | +0.5 MB |
| Shell footprint, Settings window open | +15 MB, and back within 1 MB of before 10 s after closing |
| mediad started for V3 or S12 outside a call | 40 MB (the call budget), exits within 3 s of the test ending |
| Idle CPU from any of this | 0.0 %, no new wakeups |

Measured after the review fixes (release, `strip -x`, against `be477fb` built the same way):
the shell +590,032 bytes, over its 400 KB; chatd +110,256; mediad +118,592, about six times its
20 KB. `cargo bloat` puts the shell's growth in its own code (+200 KB) and GPUI's generics
instantiated for it (+200 KB), the element trees and listeners of ten sections, not one large
item; mediad's in serde's internally tagged decoding of the new `MediaRequest` frames (`Tune`,
`Cue`: about 70 KB of `ContentDeserializer` code), the test mode (`probe.rs`) and the cues. The
notification sounds' synthesis is linked only into the example and the tests. Neither overrun
has an easy cut: the shell's would need fewer, shared closures across the rows, mediad's a
hand-written frame decoder. That measurement is history from the `settings` branch, which has merged. It is not a gate. The +400 KB shell line does not block anything on main. The seven sound files are 114 KB. Idle memory was not measured in
this pass. Re-measure only if `07-budgets-and-risk.md` grows a new gate.

What keeps idle at zero: pause and quiet hours are checked when a notification would be posted,
not by a timer. The only timer is one one-shot at the pause's end, to redraw the rail bell. No
audio device is open while nothing plays: the shell holds no audio unit after a preview ends,
chatd never links audio, and mediad does not run. The settings index is static data.

## Acceptance

One check per shipped setting, run from outside the process unless it names a unit test. "Post" means a
notification delivered by the system, read with `log stream --predicate 'subsystem ==
"com.apple.usernotifications"'` or the e2e harness's capture.

**General**
- G1: not in 0.2.35. There is no open-at-login switch and no Login Items row.
- G2: off, ⌘W: no shell and no chatd within 2 s. On, ⌘W: chatd still answers `ping`; a DM
  posts. ⌘Q in both cases: no tinyclient process within 2 s.

**Notifications**
- N1: off, inject a DM: nothing posted; `list` shows unread 1; badge 1.
- N2: DMs and mentions, inject a plain channel message: nothing posted. All messages: one post.
- N3: not in 0.2.35. There is no per-account notify menu. ND, NG, NM, and NT are global.
- N4: each value, inject a DM from "Ines" with text "hi": title, subtitle and body match the
  table exactly.
- N5: each value, inject a ring: Ring posts with the ring sound; Silently posts with nil sound;
  Don't notify posts nothing and `status` still lists the incoming call; a missed-call row
  appears after the caller stops in all three.
- N6: in a call, inject a DM: Banners only posts with nil sound; Nothing posts nothing.
- N7: pause for 1 hour, inject a DM: nothing posted, badge counts; at the end time plus 1 min a
  new DM posts. The pause survives a chatd restart. Resume posts nothing retroactively.
- N8: quiet hours covering now, inject a DM: nothing posted; a window that crosses midnight
  (22:00 to 08:00) is checked at 23:00 and 07:00 by unit test.
- N9: paused, inject a ring: on posts it, off posts nothing.
- N10: Once, window not key, DM arrives: one informational attention request (unit test on the
  call; screen recording shows one bounce). Off: none.
- N11: three unread in a muted channel, one DM: DMs and mentions shows 1; All unread shows 1 (muted
  never counts); Off shows no badge. With the shell closed, if `setBadgeCount` from chatd works.
- N13: press: one post with the current sound; with S2 at None, it posts silently.

**Sounds**
- S1: off: every post has nil sound; a join in a call produces no cue (mediad's mix log shows no
  cue frames).
- S2 to S4: the posted sound is one of Pebble, Drop, Tap, Chime, Glass, the two rings, or None. "System alert sound" is not a choice in 0.2.35.
- S5 and S6: not in 0.2.35. No Settings row or conversation-side control was found that writes them. chatd will honor a stored account or conversation sound if one exists.
- S7 to S9: each off, cause the event: no cue frames; on, exactly one cue of the right kind.
- S10: at 0 % a cue produces silent frames; at 100 % its peak is −20 dBFS ±1 dB in the mix log.
- S11: press: the sound plays once on the alert device (`AudioObjectGetPropertyData` on the
  playing device equals the system output device); after 2 s the shell has no audio unit.
- S12: with V2 set to a USB device, press: mediad opens that device, plays, exits within 3 s.
- Rate rules: five DMs in 3 s in one conversation produce one sound; DMs in two conversations
  1 s apart produce one sound.

**Appearance**
- A1: not in 0.2.35. The app is dark. Search says "tinyclient is dark only for now."
- A2: each of the five stops is selectable and lit; ⌘+ to +1 lights the +1 stop.
- A3: Compact: list row height 40 in `--shot` layout dump; 720 pt window lists at least 16 rows.
- A4: off: no preview text in any list row in `--shot`.

**Messages**
- M1: ⌘Return mode: Return in the composer adds a line and sends nothing; ⌘Return sends.
- M2: Newest: a conversation with 30 unread opens scrolled to the bottom.
- M3: off: a typing event draws nothing.
- M4: on, send ":) ok": the network receives "🙂 ok"; send "`:)`": unchanged.

**Privacy**
- P1: Discord off, type in a Discord DM: no `POST /typing` in chatd's request log. On: at most one
  per 8 s.
- P2: Teams off, read a chat: no consumption-horizon PUT; the row shows read here.
- P3: off: opening a conversation with images makes no `Fetch` request until a click.
- P4: 7 days: rows older than 7 days are gone within 1 min (`sqlite3` count is 0).

**Audio & video**
- V1, V2: set a device, start a call: `status` names it; unplug it: the call moves to the
  default, the menu says "(not connected)", the setting is unchanged.
- V3: outside a call, hold: the meter moves; release: no mediad within 3 s.
- V4: on, join: mediad sends no audio packets until unmuted; the dock shows muted.
- V5: at 50 %, a tone from the peer plays at −6 dB ±1 dB relative to 100 %.
- V6: at 60, speech at level 40 sends nothing and shows no speaking ring.

**Storage**
- D1: the total equals `du -sk` of the support folder ±1 MB.
- D2: 250 MB with 400 MB cached: under 250 MB within 1 min, least recently used gone first.
- D3: after Clear, `media/` is empty, message rows are 0, accounts still connected, drafts kept.

**Shortcuts and About**
- Shortcuts: every menu item with a key equivalent appears in the list (unit test against the
  menu table).
- B1: Copy puts the short status blurb on the clipboard and no log line. Save debug log shows `logs/bundles` or "Debug logs are turned off." It does not copy a log.
- B3: after reset, `settings` has only `version` and `conv:` rows are gone; accounts and cache
  are untouched.

**Search and keyboard**
- Searching "ding" lists S2, S3, S4. "dark" and "login" return the `NOT_YET` line (A1 and G1 are not in 0.2.35). "proxy" shows the honest line. "System alert sound" is not a result.
- With the mouse unplugged, every control that is in the window can be reached and changed from ⌘, (scripted
  keystrokes in the e2e harness).

**Budgets**: each row of the budget table, measured before and after, in the release notes.

## Later

Still absent, matching `NOT_YET` and the "Not in the window" table above. M5, V8, and V10–V19 are in the window. They are not in this list.

| Item | Waits on |
| --- | --- |
| G1 Open at login | A login item. Search says there is none yet. |
| G3 Show in menu bar | No tray icon on this commit. |
| A1 Light theme | Dark only. |
| N3 per-account notify menu | Not a row. ND, NG, NM, and NT are global. |
| N14 Reaction notifications, N15 thread replies | No reaction or thread-follow notify control. |
| N16 Focus filter | App Intents in a Rust bundle; check cost |
| S5, S6 pickers | chatd reads the scopes. No writer was found in the window. |
| S13 Custom sound files | Not offered. |
| S14 Ring-back tone, longer looping ring | Not offered. |
| M6 Spellcheck | `NOT_YET`. |
| P5 Hide from screen sharing | `NOT_YET`. |
| P6 Touch ID lock | `NOT_YET`. |
| P7 Signal always relay | `NOT_YET`. |
| V7 Camera device | "tinyclient has no camera yet." The call-dock camera toggle is `06-calls.md`. |
| Proxy, accent, font, language, sync | `NOT_YET`. A proxy, if it is ever offered, is Telegram only. It is not a setting. |

V9 is `CallGain` on a Tiny participant row, not a later Settings row.

### Never

Accent colours, a font picker, per-network theme colours, an editor for ordinary menu shortcuts, in-app banner style, an in-app Reduce Motion, a hardware-acceleration switch, an experimental tab, a language picker, a cache-location picker, and a settings sync service.

Voice processing and the push-to-talk keys are settings. They are not in this list.

### Optional Linux service management

When `tinyclient-chatd.service` is enabled in the systemd user manager, systemd
owns the daemon's lifetime. Quitting the GUI leaves chatd running independently
of the Keep receiving preference. Stop it with `systemctl --user stop
tinyclient-chatd.service` after quitting the GUI. Disabled units retain the
application-managed lifetime. See the README for setup and journal commands.
