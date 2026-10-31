# 09. Settings and sound

Every setting tinyclient offers, what each one changes, where it is kept, and which ship first.
Written 2026-10-14 from a survey of Slack, Discord, Telegram, Signal, Teams, Messages and Zed, and
an audit of this repo. This replaces the table in `docs/design/08-first-run-settings.md` §5; the
window, sizes and components there still apply.

**The rule: a control exists only when it changes real behaviour.** A setting that is not built
is left out of the window and listed under "Later" here. Nothing is greyed out, nothing says
"coming soon".

## Goals

1. **Broad.** About 50 controls in v1, covering notifications, sound, appearance, messages,
   privacy, calls, storage and startup. People who switch from Discord and Slack expect to tune
   the client, and the owner wants that.
2. **Real.** Every control has a check that someone can run from outside the process.
3. **Calm by default.** Out of the box, tinyclient notifies about DMs and mentions only and
   plays one soft sound for those, a 4 s ring for a call, and short quiet cues inside a call.
   Nothing else makes a sound.
4. **Local.** Settings live on this Mac. Nothing is fetched, synced, uploaded or reported.
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
- Toggles for echo cancellation, gain control or noise suppression. Echo cancellation is a
  behaviour that calls must have; see V-notes.
- "Experimental", "hardware acceleration" and debug switches.

## Rules this doc changes

The owner asked for sound and for breadth. These earlier rules change, on purpose:

| Rule | Where | Now |
| --- | --- | --- |
| "In-app sounds" deliberately absent | design 06 §Deliberately absent | In-call cues play in mediad on the call's own output (S7 to S10). Message sounds still go through the system notification. |
| "A ringtone or call sounds of our own" absent | design 07 §135 | One bundled ring, at most 4 s, played by the notification (S4). In-call cues as above. |
| "Custom sounds" absent | design 08 §Deliberately absent | A choice of bundled sounds is in v1. Files of your own are Later, behind a spike. |
| One sound, the system default | design 06 §6 | A sound picker per event, per network and per conversation. "System alert sound" stays a choice. |
| Dock bounce absent | design 06 §Deliberately absent | An option, default Off (N10). |
| Media cache default 1 GB | design 08 §5 | 500 MB, matching `07-budgets-and-risk.md` §6 and `CACHE_BYTES`. |
| One switch per network | design 08 §5 | A per-account level that also includes Off (N3). |

## Where settings live

Each setting has exactly one home. The Settings window reads and writes both.

| Home | What goes there | Why |
| --- | --- | --- |
| chatd: table `settings(scope, key, value)` in `chat.sqlite` | Anything chatd acts on while the window may be closed: notifications, sounds, pause, quiet hours, privacy, call devices, storage, quit behaviour | chatd posts notifications and talks to the networks; the window may not be running |
| Shell: `shell.prefs` | Anything only the window draws or reads: theme, text size, density, send key, list previews, last Settings section | It is read before the first frame, so the window opens looking right |
| The OS | Open at login (`SMAppService` state), notification permission, Focus | The OS is the source of truth; tinyclient reads it, never caches a copy |

`scope` is `""` for global, `account:<network>:<id>` for one account (one Slack workspace is one
account), and `conv:<network>:<id>` for one conversation.

Protocol, before any chatd setting ships: `Request::Settings` returns every stored value,
`Request::Set { scope, key, value: Option<String> }` stores one (`None` restores the default),
and `Change::Settings` tells every window. About 150 lines, no new dependency.

## Information architecture

The Settings window is the one in 08 §5: 720 × 560, a section list on the left, a search field at
its top, About pinned to the bottom. Sections, in order:

Accounts · General · Notifications · Sounds · Appearance · Messages · Privacy · Audio & video ·
Storage · Shortcuts · About

Columns below: **Type** is the control. **Scope** is G (global), A (per account), C (per
conversation). **Home** is `d` (chatd) or `s` (shell). **Rel** is v1 or Later.

### Accounts

As 08 §5: one group per network, a row per account with state in words, Sign out, Sign in again,
Add, reorder. New: each account row has a link, "Notifications for this account", that opens
Notifications scrolled to that account's row. No settings of its own.

### General

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| G1 | Open at login | Switch | Off until the first-load card (08 §6) asks | G | OS | Registers the bundle with `SMAppService`. At login only chatd starts; the window opens when the Dock icon or a notification is clicked | v1 |
| G2 | Keep receiving when the window is closed | Switch | On | G | d | On: closing the window hides it and chatd keeps running. Off: closing the window quits the window and chatd | v1 |
| G3 | Show in menu bar | Switch | Off | G | s | | Later |
| G4 | Install updates automatically | Switch | On | G | s | On: a newer GitHub release is downloaded while the window is open and replaces this copy on quit. Off: the notice still appears and nothing is downloaded until asked | v1 |

Caption under G2, from 08 §6: "Closing the window leaves tinyclient running. Quit with ⌘Q to
stop it." ⌘Q stops chatd and the window unless the optional Linux systemd user
service is enabled; in that mode systemd owns the daemon's lifetime (see below).

### Notifications

The top line, only when the OS has denied permission: "Notifications are turned off for
tinyclient in System Settings." with "Open System Settings".

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| N1 | Show notifications | Switch | On | G | d | Off: chatd posts no message notifications. Counts and badge keep working. Calls follow N5 | v1 |
| N2 | Notify me about | Segmented: "Direct messages and mentions" / "All messages" | DMs and mentions | G | d | Which arriving messages chatd posts. Muted conversations never notify (04 C2) | v1 |
| N3 | Per account: Notify me about | Menu: "Default" / "Direct messages and mentions" / "All messages" / "Off" | Default | A | d | Overrides N2 for one account. Off silences that account without signing out | v1 |
| N4 | Show in notifications | Segmented: "Sender and message" / "Sender only" / "Nothing" | Sender and message | G | d | Sender only: title is the sender, body "New message" or "New mention". Nothing: title is the network, body "New message". For screen sharing | v1 |
| N5 | Incoming calls | Segmented: "Ring" / "Notify silently" / "Don't notify" | Ring | G | d | Ring posts the call notification with the ring sound (S4). Silently posts it with no sound. Don't notify posts nothing; the in-window card and the missed-call row still appear | v1 |
| N6 | During a call | Segmented: "Banners only" / "Banners and sounds" / "Nothing" | Banners only | G | d | What message notifications do while I am in a call. Rings follow 07 §4 regardless | v1 |
| N7 | Pause notifications | Button with menu: "For 1 hour" / "Until tomorrow" (08:00 local) / "Until I resume"; becomes "Resume" while paused | Not paused | G | d | Nothing is posted until the stored end time; counts and badge keep counting. Also in the rail bell and the tinyclient menu | v1 |
| N8 | Quiet hours | Switch, then From and To time fields | Off; 22:00 to 08:00 | G | d | Inside the window nothing is posted except calls when N9 is on | v1 |
| N9 | Let calls ring during quiet hours and pauses | Switch | On | G | d | On: a call still posts during N7 and N8 | v1 |
| N10 | Bounce the Dock icon | Segmented: "Off" / "Once" / "Until I look" | Off | G | s | `requestUserAttention`, informational or critical, for anything that posts a notification while the window is not key | v1 |
| N11 | Dock badge counts | Segmented: "Direct messages and mentions" / "All unread" / "Off" | DMs and mentions | G | d | The number on the Dock icon. Muted conversations never count | v1 |
| N12 | Per conversation: Notifications | In Conversation › Notifications…: "Mute" (the existing network mute) | Network's state | C | network | Unchanged from today: written to the network with `SetMute` | v1 (exists) |
| N13 | Send a test notification | Button | | | | Posts one real notification, "This is how messages will look and sound", using N4 and S2. Under Focus it is silenced like any other, and the caption says so | v1 |
| N14 | Notify about reactions to my messages | Switch | Off | G | d | | Later |
| N15 | Notify about replies in threads I follow | Switch | On | G | d | | Later |
| N16 | Focus filter (show only some accounts in a macOS Focus) | App Intents | | A | OS | | Later |

Caption under the group: "Do Not Disturb you set in Slack or Discord is followed here. For
schedules by place or app, use Focus in System Settings; tinyclient follows it."

Behaviour with no setting, all v1, all required before N1 ships (from 04 N1 to N7 and design 06
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
| S1 | Play sounds | Switch | On | G | d | Off: every notification posts with no sound and mediad plays no cues. The test buttons still play | v1 |
| S2 | Message sound | Picker: 5 bundled sounds, "System alert sound", "None" | "Pebble" | G | d | The sound on a message notification | v1 |
| S3 | Mention sound | Picker: "Same as messages", the 5 sounds, "System alert sound", "None" | Same as messages | G | d | The sound when the notification is for a mention, including in "All messages" mode, so a mention is heard apart from chatter | v1 |
| S4 | Ring | Picker: 2 bundled rings, "System alert sound", "None" | "Soft ring" | G | d | The sound on the incoming-call notification | v1 |
| S5 | Per account: Message sound | Menu: "Default", the 5 sounds, "System alert sound", "None" | Default | A | d | Overrides S2 and S3 for one account, so work and friends sound different | v1 |
| S6 | Per conversation: Sound | In Conversation › Notifications…: "Default", the 5 sounds, "System alert sound", "None" | Default | C | d | Overrides S5 for one conversation. Local only; the network never sees it | v1 |
| S7 | In calls: someone joins or leaves, or I am moved | Switch | On | G | d | mediad plays the join, leave or moved cue | v1 |
| S8 | In calls: I mute, unmute, deafen or undeafen | Switch | On | G | d | mediad plays the matching cue | v1 |
| S9 | In calls: connection lost, back or ended | Switch | On | G | d | mediad plays the matching cue; "ended" is being kicked, banned, timed out or the room deleted | v1 |
| S10 | Call sound volume | Slider 0 to 100 % | 50 % | G | d | Gain on the cues in S7 to S9 only. Voices are not affected | v1 |
| S11 | Play | A small play button beside S2, S3, S4, S5 | | | | Plays that sound once, on the device the notification would use (below) | v1 |
| S12 | Play test sound | Button beside S10 | | | | mediad plays the join cue at S10's volume on the call speaker (V2) | v1 |
| S13 | Add a sound file… | Picker entry | | G | d | | Later (spike) |
| S14 | Ring-back while I am calling someone | Switch | | G | d | | Later |

Caption under S2 to S4: "Notification sounds use the alert volume in System Settings › Sound."

### Appearance

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A1 | Theme | Segmented: "Match system" / "Dark" / "Light" | Match system | G | s | The token set of `01-system.md` §2. Applies in the next frame, every window, no restart | v1 |
| A2 | Text size | Five stops: −1 to +3 (body 13, 14, 15, 16, 17) | 0 (14) | G | s | Type sizes and `row_height`. Same state as ⌘+ ⌘− ⌘0, so the control never shows nothing selected | v1 |
| A3 | Density | Segmented: "Comfortable" / "Compact" | Comfortable | G | s | Compact: list rows 48 → 40, avatars 28 → 24, 2 pt less between message groups | v1 |
| A4 | Show message previews in the list | Switch | On | G | s | Off: list rows show the name and time only. For screen sharing | v1 |
| A5 | Accent colour, fonts, chat backgrounds, per-network colour | | | | | | Never (08) |

Reduce Motion, Increase Contrast and Reduce Transparency are read from macOS and honoured, with
no control.

### Messages

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| M1 | Send with | Segmented: "Return" / "⌘Return" | Return | G | s | Return: Return sends, ⇧Return adds a line. ⌘Return: Return adds a line, ⌘Return sends. ⌥Return still sends silently. Bindings change at once | v1 |
| M2 | Open conversations at | Segmented: "First unread" / "Newest message" | First unread | G | s | Where the message list lands on open. First unread is today's behaviour (07 R3) | v1 |
| M3 | Show when others are typing | Switch | On | G | s | Off: no typing line is drawn. What I send is P1 | v1 |
| M4 | Convert typed emoticons to emoji | Switch | Off | G | s | On send, `:)` `:(` `:D` `;)` `<3` standing alone become 🙂 🙁 😀 😉 ❤️. Never inside code spans | v1 |
| M5 | Play GIFs and animated stickers | Segmented: "Automatically" / "On hover" / "Never" | Automatically | G | s | Automatically: every GIF, animated WebP, APNG and animated emoji on screen plays while the window is key, within the 07 budgets. On hover: a still with a GIF badge, playing under the pointer. Never: a still with a badge and a play disc; a click opens it in the viewer, which plays it. Reduce Motion makes Automatically play under the pointer. Kept in `shell.prefs` | v1 |
| M6 | Check spelling | Switch | | G | s | | Later |

### Privacy

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | Let others see when I am typing | Per account switch; fixed rows for Telegram and Signal | Discord off, Slack on, Teams on. Telegram: "Always on", caption "Telegram does not let an app turn this off." Signal: "Set on your phone" | A | d | Off: the adapter sends no typing signal | v1 |
| P2 | Let others see what I have read | Teams switch; fixed rows for Telegram and Signal | Teams on. Telegram "Always on". Signal "Set on your phone" | A | d | Teams off: no consumption-horizon update is sent; the chat is still marked read here. Caption: "Off also leaves chats unread on your other devices." Group caption: "Discord and Slack have no read receipts." | v1 |
| P3 | Load images and link previews automatically | Switch | On | G | s | Off: each image or preview is one line with its size; a click loads it. Nothing is fetched until clicked | v1 |
| P4 | Keep messages on this Mac for | Segmented: "7 days" / "30 days" / "90 days" | 90 days | G | d | The age half of the retention rule (`store.rs`); the 2,000 per conversation cap stays. Pruned within 1 min of the change and daily | v1 |
| P5 | Hide tinyclient from screen sharing and recordings | Switch | | G | s | | Later (spike: macOS 15 capture may ignore `sharingType`) |
| P6 | Lock with Touch ID | Switch | | G | s | | Later |
| P7 | Signal: always relay calls | Switch | | A | d | | Later, with Signal calls |

### Audio & video

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| V1 | Microphone | Menu: "Automatic (name)", "System default (name)", hardware, then virtual devices marked "(virtual)" | Automatic | G | d | The same choice as the call's device menu, stored by name and the system's id (WASAPI endpoint id, CoreAudio UID). chatd sends it to mediad at start and whenever it changes, mid-call too. Automatic is the system default unless that is a virtual device (Krisp, VB-Cable, Voicemeeter, NVIDIA Broadcast, Steam Streaming), then a headset or speakers; System default follows the system live. A missing or failing device is stood in for, said in words, and switched back to when it returns; the menu says "(not connected)" | v1 |
| V2 | Speaker | Menu, as V1 | Automatic | G | d | As V1, for output. Also where S12 plays | v1 |
| V3 | Test microphone | Button, "Test" / "Stop" (a toggle, design 09 §4) | | | | While on, the 20-segment meter is live, for at most 30 s. Outside a call, mediad starts for it (`mediad test`) and exits within 3 s of Stop | v1 |
| V4 | Join calls with my microphone muted | Switch | Off | G | d | On: every join and every Accept starts muted, and the dock shows it | v1 |
| V5 | Call volume | Slider 0 to 100 % | 100 % | G | d | Gain on everyone's voice in mediad's mix. 100 % is today's level | v1 |
| V6 | Microphone sensitivity | Slider over the live meter, with "Default" mark | 25 (today's `VOICE`) | G | d | The voice gate threshold in mediad: below it, nothing is sent and no speaking ring shows | v1 |
| V7 | Camera | Menu | | G | d | | Later, with camera |
| V8 | Microphone opens by | Segmented: Voice activation / Push to talk (Settings › Voice) | Voice activation | G | d | `push_to_talk`: mediad's `Input` is push-to-talk with the V14 delay instead of the V6 gate. A Tiny room that requires it (or where voice activation is not allowed) is push-to-talk whatever this says | v1 |
| V9 | Per-person volume | Row menu in the participant list | 100 %, heard | A | d | `CallGain {account, user_id, percent, muted}`, kept as `gain:<user id>` in the account's scope and sent to mediad as `Gain`, 0 to 200 % | chatd and mediad done; the row menu is the participant list's (07 §7a) |
| V10 | Echo cancellation | Switch | On | G | d | `echo_cancellation`, in mediad's `Processing`: WebRTC AEC3 (sonora) with the call's mix as its reference | v1 |
| V11 | Noise suppression | Switch | On | G | d | `noise_suppression`, as V10 | v1 |
| V12 | Automatic gain | Switch | On | G | d | `auto_gain`, as V10 (AGC2, adaptive digital) | v1 |
| V13 | Push-to-talk key | Key recorder | None | G | s | `ptt_key`, a `hotkey::Binding` (`f13`, `ctrl+shift+t`, `mouse4`, `ctrl`). The shell watches it in any app and sends `CallTalk` | v1 |
| V14 | Release delay | Stepped slider: 0, 100, 200, 300, 500 or 1000 ms | 200 ms | G | d | `ptt_release_ms` | v1 |
| V15 | Priority push-to-talk key | Key recorder | None | G | s | `priority_key`; `CallTalk {priority}`, and `call.state` `priority` on a Tiny server | v1 |
| V16 | Toggle mute key | Key recorder | None | G | s | `mute_key`; does what ⌘⇧M does, in any app | v1 |
| V17 | Toggle deafen key | Key recorder | None | G | s | `deafen_key`; as ⌘⇧D | v1 |
| V18 | Hear yourself while testing | Switch | Off | | | The window's own: V3's test plays the processed microphone back (`TestMicrophone {loopback}`, mediad `Loopback`), outside a call | v1 |

The device menu in the call dock gains "Play test sound" (S12) and "Call settings…", which opens
this section. Picking a device in the dock changes the call and not the default; the dock marks
what is in use and resets when the call ends (fixes the stale-device defect).

V-notes, behaviour with no setting: on macOS, calls move to the VoiceProcessingIO audio unit for
echo cancellation, gain and noise reduction, which also brings the system's Mic Mode. Opus moves
to `OPUS_APPLICATION_VOIP`. V6's default may change after VPIO; the stored value is kept if the
user moved it.

### Storage

| # | Setting | Type | Default | Scope | Home | What it changes | Rel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| D1 | Usage | Total, two-part bar (Messages, Images and files), "Show in Finder" | | | | Read from `Store::media_on_disk` and the database file size | v1 |
| D2 | Keep images and files up to | Segmented: 250 MB / 500 MB / 1 GB / 2 GB | 500 MB | G | d | `Blobs::evict`'s cap, oldest unused first; takes effect within 1 min | v1 |
| D3 | Clear… | Button, then confirm (08 §5 copy) | | | | Deletes `media/` and message rows, then vacuums. Sessions, drafts and settings stay | v1 |

### Shortcuts

A read-only list of every shortcut, grouped as the menus are, searchable. ⌘/ opens it. Caption:
"To change a shortcut, add it in System Settings › Keyboard › Keyboard Shortcuts › App
Shortcuts." Every command is in a menu, so that works for all of them. v1. A key editor is Never.

### About

As 08 §7, with the Signal fix: "One small app for Slack, Discord, Telegram, Teams and Signal."
Added, all v1:

| # | Control | What it does |
| --- | --- | --- |
| B1 | Copy diagnostics | Version, OS, service state, and account connection states. No message text, names, tokens, or log lines. Save debug log writes the in-memory ring under `logs/bundles/` and shows that directory |
| B2 | Show logs in Finder | Opens `logs/` (`tinyclient.log`, `chatd.log`, `mediad.log`, `login.log`, and `bundles/`) |
| B3 | Reset all settings… | Confirm: "Reset every setting to its default? Accounts, messages and files are not touched." Deletes every row in `settings` with global or account scope and the settings lines in `shell.prefs`. Per-conversation sounds go too |

### Count

v1: 50 controls in the window (G 2, N 12, S 12, A 4, M 4, P 4, V 6, D 3, B 3, Accounts and
Shortcuts besides), plus per-account rows that multiply with accounts and per-conversation sound.
Later: 18, listed below.

## Sound design

### Two paths, no third

| Path | Plays | How | Cost |
| --- | --- | --- | --- |
| The notification | Message, mention and ring sounds | chatd sets `UNNotificationContent.sound` to a named file in `tinyclient.app/Contents/Resources` (macOS looks in the main bundle), to `.default` for "System alert sound", or to nil | 0 bytes of audio code, 0 memory. macOS applies Focus, the alert volume, the alert device and its per-app "Play sound for notifications" switch |
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

- Notification sounds: no slider. They follow the macOS alert volume, and a second slider would
  disagree with it. The caption says where it is.
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
- **Shell:** `shell.prefs` gains one line per setting (`theme`, `density`, `send`, `previews`,
  `open_at`, `settings_section`, and so on). Today an unknown line is dropped when the file is
  rewritten; that changes so unknown lines are kept, and an older build never erases a newer
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
hand-written frame decoder. **The branch is not to merge past these budgets without the owner's
sign-off recorded in `07-budgets-and-risk.md`.** The seven sound files are 114 KB. Idle memory was not measured in
this pass.

What keeps idle at zero: pause and quiet hours are checked when a notification would be posted,
not by a timer. The only timer is one one-shot at the pause's end, to redraw the rail bell. No
audio device is open while nothing plays: the shell holds no audio unit after a preview ends,
chatd never links audio, and mediad does not run. The settings index is static data.

## Acceptance

One check per v1 setting, run from outside the process unless it names a unit test. "Post" means a
notification delivered by the system, read with `log stream --predicate 'subsystem ==
"com.apple.usernotifications"'` or the e2e harness's capture.

**General**
- G1: switch on, log out and in: `chatd` is in the process table within 10 s and the shell is
  not; System Settings › Login Items lists tinyclient. Off: neither after login.
- G2: off, ⌘W: no shell and no chatd within 2 s. On, ⌘W: chatd still answers `ping`; a DM
  posts. ⌘Q in both cases: no tinyclient process within 2 s.

**Notifications**
- N1: off, inject a DM: nothing posted; `list` shows unread 1; badge 1.
- N2: DMs and mentions, inject a plain channel message: nothing posted. All messages: one post.
- N3: account at Off, the same DM: nothing posted; another account's DM still posts.
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
- S2 to S6: for each layer set, inject the matching DM or mention: the posted content's sound
  name equals the expected file, resolved conversation over account over mention over message.
- S7 to S9: each off, cause the event: no cue frames; on, exactly one cue of the right kind.
- S10: at 0 % a cue produces silent frames; at 100 % its peak is −20 dBFS ±1 dB in the mix log.
- S11: press: the sound plays once on the alert device (`AudioObjectGetPropertyData` on the
  playing device equals the system output device); after 2 s the shell has no audio unit.
- S12: with V2 set to a USB device, press: mediad opens that device, plays, exits within 3 s.
- Rate rules: five DMs in 3 s in one conversation produce one sound; DMs in two conversations
  1 s apart produce one sound.

**Appearance**
- A1: Match system, switch macOS to Light: the next frame uses light tokens (screenshot with
  `--shot` in both); a grep finds no colour literal outside `theme.rs`; every text token meets the
  ratios in `01-system.md` §2.
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
- V1, V2: set a device, start a call: `status` names it; unplug it: the call moves to a
  stand-in (never a virtual device), the dock and menu say so, the setting is unchanged; plug it
  back in: the call moves back within 2 s. Pick a device in the call's menu, leave, quit,
  rejoin: the call uses it.
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
- Searching "ding" lists S2, S3, S4; "dark" lists A1; "proxy" shows the honest line.
- With the mouse unplugged, every v1 control can be reached and changed from ⌘, (scripted
  keystrokes in the e2e harness).

**Budgets**: each row of the budget table, measured before and after, in the release notes.

## Release cut

### Built so far (2026-10-14)

30 of the 50 v1 controls are in the window, each with its check, plus Accounts, Shortcuts and
search:

- **General:** G2, with `Request::Shutdown`. ⌘Q now stops chatd too.
- **Sounds:** S1, S7 to S10, S12. The in-call cues are mixed in mediad; S1 governs the cues only
  until notifications post sounds.
- **Appearance:** A2 (five stops, the ⌘+ state), A3, A4.
- **Messages:** M1 to M4.
- **Privacy:** P1 and P2 per account, P3 (pictures, stickers and link-preview images in
  messages; avatars and custom emoji still load), P4 with its count.
- **Audio & video:** V1 to V6. The device menus list devices by running `mediad --devices`,
  which opens nothing and exits; the shell links no audio code.
- **Storage:** D1 to D3. **About:** B1 to B3.
- **Defects fixed:** the stale call device (a pick in the call's menu ends with the call); Discord
  typing now follows P1, off by default; Teams receipts follow P2; ⌘Q stops chatd; text size has
  five lit stops; About names Signal.
- **Sounds:** the seven bundled files are rendered and committed, checked against a manifest on
  macOS. They are not copied into the bundle until notifications post them.

### Notifications built (2026-10-28)

Step 6 is in, on Windows first. chatd posts (`chatd/src/notify.rs`), through the `notifier` crate:
WinRT toasts on Windows, UNUserNotificationCenter on macOS (inside the bundle only), and
org.freedesktop.Notifications on Linux.

- **Notifications:** N1, N2, N4 (and its locked-screen twin, read from the session's WTS lock
  state on Windows), N5, N6, N7 (1 hour, until 08:00, until resumed), N8 with its two times,
  N9, N10 (taskbar flash on Windows, Dock bounce on macOS), N11 (the taskbar overlay or Dock
  badge, the `ATTENTION` total), N13. Each kind has a switch of its own: direct messages, group
  chats, mentions in channels, Tiny servers. N3 and the per-conversation level are honoured by
  chatd (`notify_about` in the account's or conversation's scope); N3 has no row yet.
- **Behaviour with no setting:** all of the list above the Sounds table: never the conversation
  open in the focused window (`Watching.focused`), never mine, never history at connect (older
  than 2 minutes when heard), never muted; one notification per conversation, its newest three
  lines, replaced in place and withdrawn when the row is read anywhere; Reply and Mark as read;
  a click opens the window there, starting it if it is closed; the network's Do Not Disturb (my
  own presence `dnd`) holds it back. The ring is posted with Accept and Decline and withdrawn
  when it stops.
- **Sounds:** S1 now silences notifications too; S2, S3, S4 with S11's play buttons; S5 and S6
  are honoured, and S6 is set in Details beside the conversation's notifications. Windows and
  Linux post silent and chatd plays the sound itself (`mediad --sound`, rendered from the same
  code as the files), at a notification volume of its own (`alert_volume`, shown where it
  applies). The silence rules are applied in the order above.
- A scratch directory (`TINYCLIENT_DIR`) posts nothing unless `TINYCLIENT_NOTIFY=on`, so tests
  never do. `tinyclient --notify-test` posts N13 from a terminal.

Not built yet, and so absent from the window: G1 (needs a login item that starts chatd alone, in
the signed bundle), N3's rows, the permission banners of design 06, A1 (step 7: light tokens and
contrast), the Accounts row menu, the menu bar's Notifications… popover (Details carries its two
controls), and VPIO with `OPUS_APPLICATION_VOIP` (V-notes). "System alert sound" is not offered:
an unpackaged Windows app could only name the system's sounds, and chatd plays ours.
`packaging/bundle.sh` copies the seven CAF files into the bundle's Resources, where macOS finds
them by name. Search answers "theme", "login" and the Later terms with a line saying so.

### v1, build now

Order matters; each step ships the rows it makes real, and no row appears before its check passes.

1. **Defects first.** Stale call device; Discord typing always sent; Teams receipts always sent;
   ⌘Q not stopping chatd; text size showing nothing selected; About leaving out Signal.
2. **Settings store and protocol** (`settings` table, `Settings`, `Set`, `Change::Settings`),
   `shell.prefs` keeping unknown lines, search, keyboard, the Shortcuts section, About B1 to B3.
3. **Cheap and fully real:** A2, A3, A4, M1 to M4, P1 to P4, D1 to D3.
4. **Calls:** V1 to V6, S7 to S10, S12; VPIO and `OPUS_APPLICATION_VOIP` alongside.
5. **General:** G2 (with `Request::Shutdown`), then G1 in the signed bundle.
6. **Notifications and their sounds**, after the spike in `04-live-signals.md` §Decision: N1 to
   N13, S1 to S6, S11, the seven bundled sounds. If the spike needs the helper app fallback, the
   rows are the same; only the poster changes.
7. **A1, light theme**, once the 23 hard-coded colours go through tokens and the light values
   pass contrast.

v1 is 50 controls. If a step slips, its rows stay out and the rest still ship.

### Later

| Item | Waits on |
| --- | --- |
| G3 Show in menu bar | Design of the menu; low demand while the Dock badge works |
| N14 Reaction notifications, N15 thread replies | Adapters telling reactions to my messages and followed threads apart, per network |
| N16 Focus filter | App Intents in a Rust bundle; check cost |
| S13 Custom sound files | The `~/Library/Sounds` spike |
| S14 Ring-back tone, longer looping ring | mediad starting at ringing out; the withdrawal-stops-sound check |
| M5 GIF playback choice | GIF playback |
| M6 Spellcheck | About 300 lines on `NSSpellChecker`, macOS only |
| P5 Hide from screen sharing | Proof it works on macOS 15 and later |
| P6 Touch ID lock | Design of what is locked (window only; chatd keeps running) |
| P7 Signal always relay | Signal calls |
| V7 Camera, V8 push to talk, V9 per-person volume | Camera; Input Monitoring permission and a key recorder; participant list menu |
| Proxy (SOCKS5 for Telegram) | Wanted in censored regions; TDLib supports it. Breaks rule 10 of `07-budgets-and-risk.md` §5 for the other networks, so Telegram only |
| Keyword alerts of our own | The network's own keyword lists are honoured already |
| Noise suppression model (`nnnoiseless`, about 100 KB) | Evidence that VPIO is not enough |

### Never

Accent colours, fonts, per-network colours, a keybinding editor, in-app banner style or alert
volume, an in-app Reduce Motion, toggles for echo cancellation or gain, hardware acceleration,
"experimental" options, language, cache location, notification rules editors beyond the levels
above, a settings sync service.

### Optional Linux service management

When `tinyclient-chatd.service` is enabled in the systemd user manager, systemd
owns the daemon's lifetime. Quitting the GUI leaves chatd running independently
of the Keep receiving preference. Stop it with `systemctl --user stop
tinyclient-chatd.service` after quitting the GUI. Disabled units retain the
application-managed lifetime. See the README for setup and journal commands.
