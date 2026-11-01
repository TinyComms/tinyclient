# 09. Settings and sound

Requirements: `docs/product/09-settings.md` (setting IDs such as N3 and S6 are from there). No mockup yet; the shot hook is `--settings <section> --settings-shot`. All sizes are points. Colors and type are `theme.rs` tokens from `01-system.md`; new ones are listed at the end. Strings in quotes are final copy. This replaces `08-first-run-settings.md` §5. Where 08 gives different sizes (17 pt heading, radius 12 groups, 44 rows, 32 pt list items with radius 8), `01-system.md` wins and the values below apply.

## 1. Window

A separate window, 720 × 560, not resizable, not minimizable, transparent title bar, traffic lights at 12, 16. ⌘, opens it on the last section used (`settings_section` in `shell.prefs`), Accounts the first time. ⌘W and Esc close it. The window title, read by Mission Control and VoiceOver, is the section name. Every control applies at once: no Save, no Apply, no restart.

| Part | Spec |
|---|---|
| Section list | 184 wide, `sidebar`, 1 pt `hairline` on its right. Top strip 52 (the traffic lights), then the search field, then sections |
| Search field | 01 search field, 168 wide (inset 8), 28 high. Placeholder "Search settings". Empty and unfocused it shows a `⌘F` keycap; with a query, a clear × |
| Section item | One-line row: 28 high, radius 6, padding 8, glyph 16, gap 8, 13/18. 2 between items, 8 below the search field. Rest: label `text_soft`, glyph `text_faint`. Hover: `hover` fill. Selected: `selected` fill, label and glyph `text` |
| About | Pinned 12 from the list's bottom |
| Pane | `canvas`, 536 wide. Strip 52 holds the section heading, 16/22 600 `text`, padding-left 28, centred vertically. Content column 480, padding 0 28 28, scrolls under the strip with the 01 overlay scrollbar |

**Sections, glyphs and ⌘ keys**

| ⌘ | Section | Glyph | New glyph? |
|---|---|---|---|
| 1 | Accounts | `users` | |
| 2 | General | `settings` | |
| 3 | Notifications | `bell` | |
| 4 | Sounds | `volume-2` | |
| 5 | Appearance | `sun-moon` | new |
| 6 | Messages | `message-square` | |
| 7 | Privacy | `lock` | |
| 8 | Audio & video | `mic` | |
| 9 | Voice | `headphones` | |
| | Storage | `hard-drive` | new |
| | Shortcuts | `command` | new |
| | About | `info` | |

⌘1 through ⌘9 reach Accounts through Voice. Storage, Shortcuts, and About have no digit.

Height check: 52 + 28 + 8 + 11 × 30 = 418. About stays pinned 12 from the list's bottom. A section whose rows are all out of this build (none in v1) is absent from the list; it is never shown empty.

## 2. Page anatomy

| Element | Spec |
|---|---|
| Group label | 11/14 500 `text_faint`, 24 high, padding-left 12, directly above its card. Optional; the first group of a section usually has none |
| Group card | `raised`, radius 8, no border (a step, not a line: 01 rule 4). Rows inside are split by 1 pt `hairline` inset 12 on both sides |
| Row, one line | Min height 40, padding 0 12. Label 13/18 400 `text` on the left; control right-aligned; at least 16 between them; both centred vertically |
| Row, two lines | Min height 52: 8, label 13/18, 2, second line 12/16 `text_soft`, 8. The second line says what the current value does, and changes with it |
| Reset slot | 28 reserved left of every control that has a default: a 24 × 24 icon button and a 4 gap. See §3.10 |
| Group caption | 12/16 `text_faint`, 6 below the card, padding 0 12, max 456. One or two sentences. An action in a caption is a link, 12/16 `accent_text`, underlined on hover |
| Spacing | 20 between groups. 16 between a banner and the first group |
| Banner | 01 inline banner, `raised`, radius 6, min height 40, padding 8 12, glyph 16 in its semantic color, text 13/18 `text`, optional small (24) secondary button at the right. Sits at the top of the content column |

Labels wrap to a second line rather than truncate; the row grows. At text step +3 every type size is +3 and row minimums are +6 (40 → 46, 52 → 58), as the main window's 28 rows grow +2 per step. The content column stays 480.

## 3. Controls

States are rest / hover / pressed / focus / disabled as in 01 §3. Focus is keyboard only: 2 gap then a 2 pt `focus` ring. Disabled is 50% opacity and appears only in the error states of §6; a feature that is not built is absent, never disabled.

### 3.1 Switch
32 × 18, radius 9, knob 14 white, inset 2. Off: track `switch_off`. On: `accent`. Hover: off track `border_strong`, on track `accent_hover`. The knob moves in the same frame; switches do not slide (01: state changes never animate). The whole row is the click target, so clicking the label toggles it. Focus ring around the track, radius 11. Role `Switch`, label = row label.

### 3.2 Segmented
01 component: 28 high, padding 2, segments 24, 13/500. Two to four choices, used only when the labels sum to 260 or less; longer choice sets use a dropdown. The segments keep their widths whichever is selected. ← → move the choice. Role `RadioGroup` of `RadioButton`.

### 3.3 Slider with value
| Part | Spec |
|---|---|
| Track | 200 wide (238 in the level-meter row), 4 high, radius 2. Value side `accent`, rest `switch_off` |
| Thumb | 16 circle, white, 1 pt black 12% edge, shadow 0 1 3 0 black 30%. Hit box 28 high over the full track width |
| Ticks (stepped sliders) | 2 × 6, `border_strong`, 4 below the track, one per stop. The default stop's tick is 2 × 8 `text_faint` with the tooltip "Default" |
| Value | 13/18 tabular `text_soft`, right-aligned in a 48 box, 8 right of the track: "50 %", "14 pt", "25" |

Click on the track jumps there; drag follows the pointer with no easing; stepped sliders snap. Keys: ← → by one step (1 % or one stop), ⌥← ⌥→ by 10, Home and End to the ends. Values are stored when the pointer is released or 300 ms after the last key, so a drag is one write. Role `Slider` with min, max, step and a value text equal to the label.

### 3.4 Dropdown
A popup button: 01 secondary button look, 28 high, radius 6, padding 10 left and 8 right, label 13/500, `chevron-down` 14 `text_soft` at the right. Width is the widest choice plus 32, clamped 120 to 240, so the button never changes width with the value. It opens a 01 context menu 4 below, left edges aligned, at least as wide as the button; the current item carries `check` 16. A long menu scrolls past 10 items. Keys: Space, Return or ↓ open; arrows move; Return picks; Esc closes; typing a letter jumps. Role `ComboBox` (AXPopUpButton), value = the shown label.

**Inherited versus set.** In an override row (§3.9) the first item is "Default (Direct messages and mentions)", naming what it inherits, then a separator, then the values. While the row inherits, the button shows the inherited value in `text_soft`; once set here, the label is `text` and the reset slot shows. Grey means "comes from above"; white means "chosen here".

### 3.5 Device picker
A dropdown (3.4) of audio devices. First item "System default (MacBook Pro Microphone)", naming today's default; separator; each device by its system name. A stored device that is unplugged stays in its place as "Studio Display Microphone (not connected)" in `text_soft`, and the button shows the same words; the row's second line reads "Calls use the system default until it is back." No devices at all: the button reads "No microphone found" and its menu has that one disabled item. The list updates live while the menu is open.

### 3.6 Level meter
20 segments, 10 × 8, radius 2, 2 apart: 238 wide. Unlit `switch_off`. Lit below the sensitivity line `text_faint`; lit at or above it `positive`, so the meter shows what would be sent. The sensitivity line is a 2 × 14 `text` bar at the threshold, centred on the meter. The V6 slider sits 6 below at the same width, so its thumb is under the line. Levels arrive at 10 Hz only while a test runs; nothing smooths them and there is no peak hold. Not running: all segments unlit, the line still drawn. Role `Meter`, value = level 0 to 100.

### 3.7 Sound picker with play
A dropdown (3.4), fixed 160 wide, then 4, then an icon-only ghost button 28 × 28 with `play` 16. Menu items show the name and, right-aligned in the shortcut slot, a two-word character in 12 `text_soft` (`text_faint` may not sit on `overlay`): "Pebble   soft, low". Order: "Same as messages" (S3 only) or "Default (Pebble)" (override rows), separator, the five sounds (rings: two), separator, "System alert sound", "None".

The play button plays the selected sound once; while it plays the glyph is `square` 14 and the tooltip "Stop". Picking a new sound from the menu plays it once too, as macOS Sound settings do; moving through the open menu does not. One sound at a time: a new play stops the last. With "None" selected, the play button is absent and its 32 stays reserved. Tooltip: "Play Pebble". These buttons and the play-on-pick exist only if S11 ships (product: alert-device routing); otherwise the picker sits alone, right-aligned. Role `Button`, label "Play Pebble".

### 3.8 Shortcut recorder (not in v1)
Specified for V8 push to talk, which is Later; no recorder is in the v1 build, and the Shortcuts section is read-only. An input 140 × 28, showing the shortcut as keycaps (18 high, radius 4, `border`, 11/500 `text_soft`, padding 0 4, 2 apart) or "Click to record" in `text_faint`. Click or Return: border `focus`, text "Press a key…". The first key with or without modifiers is taken; Esc cancels; ⌫ clears to none. A clash: 12 `danger` line below, "⌘⇧M is already Mute.", and the old value stays. A global key that needs Input Monitoring shows the banner of §6 when recorded.

### 3.9 Per-network override row
One row per signed-in account, in rail order, grouped under a label such as "Each account". Leading: the network's mark 16 in full color (01 Icons), 8 gap, then the account name 13/18 `text` ("Lumen Labs", "mira.k"). Two accounts on one network show the network in a second line: "Slack · mira@halden.example". Trailing: the same control as the global row, as an inheriting dropdown (3.4), or a switch where the setting has no global value (P1, P2). Accounts that are signed out keep their row, mark at 40% opacity. No accounts: the group is absent.

**Fixed rows** say what the network decides. The control's place holds the value as 13/18 `text_soft` ("Always on", "Set on your phone"), there is no reset slot, and the second line names the reason: "Telegram does not let an app turn this off."

### 3.10 Reset to default
The reset slot holds an icon-only ghost button, 24 × 24, `rotate-ccw` 14 `text_faint` (hover `text`), drawn only while the value differs from its default. The 28 is reserved on every resettable row whether or not the button shows, so controls stay in one column and nothing shifts. Tooltip and label: "Reset to default (Pebble)". It sends `Set { value: None }`; on an override row that means "inherit again". Buttons, links and fixed rows have no slot. Reset all is About › "Reset all settings…" (B3).

### 3.11 Time field
An input 64 × 28 (84 when macOS uses a 12-hour clock), tabular 13/18, centred text: "22:00" or "10:00 PM", following the macOS clock setting. Hours and minutes are two segments; ← → move between them; ↑ ↓ step hours by 1 and minutes by 5; digits type over. Role `TimeInput`.

### 3.12 Button with menu
A secondary button with `chevron-down` 14 after its label, 6 gap (N7 "Pause"). Opens a 01 context menu below it.

## 4. Sections

Rows are listed top to bottom: **Group** (label, if any) · **Row** label · **Control** · second line · and group caption after. Values in the product doc are not repeated where they are the same.

### Accounts
As 08 §5 and §6 with 01 sizes: one group per network labelled with the network name; each account a two-line row (avatar 32, name, "Slack · Connected" with a 6 pt state dot), then an icon-only `ellipsis` button and "Sign out…". The `ellipsis` menu: "Notifications for this account" and "Sound for this account", which open Notifications or Sounds with that account's row highlighted (§5, search jump). Caption: "Signing out removes this account's sign-in and everything tinyclient saved for it from this Mac."

### General
| Row | Control | Second line |
|---|---|---|
| "Keep receiving when the window is closed" (G2) | Switch, on. macOS only. | On: "Closing the window leaves tinyclient running. Quit with ⌘Q to stop it." |
| "Install updates automatically" (G4) | Switch, on | On: a newer version downloads while the app is open and installs when you quit. |

Caption, when G2 is off: "Closing the window quits tinyclient. Nothing arrives until you open it again." Elsewhere the G2 row is absent and closing quits (`06` §8).

### Notifications
| Group | Row | Control | Second line |
|---|---|---|---|
| | "Show notifications" (N1) | Switch | Off: "Counts and the Dock badge keep working." |
| | "Notify me about" (N2) | Dropdown: "Direct messages and mentions" / "All messages" | |
| | "Show in notifications" (N4) | Dropdown: "Sender and message" / "Sender only" / "Nothing" | A sample of the result: "Ines Ruiz: Are we still on for 3?" / "Ines Ruiz: New message" / "Slack: New message" |
| Calls | "Incoming calls" (N5) | Dropdown: "Ring" / "Notify silently" / "Don't notify" | Ring: "Plays Soft ring, at most 4 seconds." (the S4 name). Don't notify: "Calls still show in the window, and missed calls are listed." |
| Calls | "During a call" (N6) | Dropdown: "Banners only" / "Banners and sounds" / "Nothing" | |
| Each account | One row per account (N3) | Inheriting dropdown: "Default (…)" / "Direct messages and mentions" / "All messages" / "Off" | Off: "Signed in, but silent." |
| Quiet time | "Pause notifications" (N7) | Button with menu "Pause": "For 1 Hour" / "Until Tomorrow" / "Until I Resume" | Paused: the label becomes "Paused until 3:00 PM" ("Paused until you resume") and the control a secondary "Resume" |
| Quiet time | "Quiet hours" (N8) | Switch | |
| Quiet time | "From" (N8, only while on) | Two time fields: From [22:00] "to" [08:00] | "Every day. Crosses midnight." when it does |
| Quiet time | "Let calls ring during quiet hours and pauses" (N9) | Switch | |
| Dock | "Dock badge counts" (N11) | Dropdown: "Direct messages and mentions" / "All unread" / "Off" | |
| Dock | "Bounce the Dock icon" (N10) | Segmented: "Off" / "Once" / "Until I look" | "Only when tinyclient is in the background." |
| Test | "Send a test notification" (N13) | Secondary button "Send" | After a press, for 8 s: "Sent. If nothing appeared, check Focus and System Settings › Notifications." |

Captions. Under the first group: "Conversations you muted never notify." Under Each account: "Do Not Disturb you set in Slack or Discord is followed here." Under Quiet time: "For schedules by place or app, use Focus in System Settings. tinyclient follows it." with the link "Open Focus Settings". The From row is absent while Quiet hours is off; it is not greyed.

### Sounds
| Group | Row | Control | Second line |
|---|---|---|---|
| | "Play sounds" (S1) | Switch | Off: "Notifications arrive silently and calls make no sounds. Your choices below are kept." |
| Notification sounds | "Message sound" (S2) | Sound picker | |
| Notification sounds | "Mention sound" (S3) | Sound picker, first item "Same as messages" | |
| Notification sounds | "Ring" (S4) | Sound picker: "Soft ring" / "Classic ring" / "System alert sound" / "None" | |
| Each account | One row per account (S5) | Sound picker, inheriting | |
| Conversations | One row per conversation with its own sound (S6): avatar 20, title 13/18 | Sound picker, inheriting; reset removes the row | |
| In calls | "Someone joins or leaves" (S7) | Switch | |
| In calls | "I mute, unmute or deafen" (S8) | Switch | |
| In calls | "Connection lost or back" (S9) | Switch | |
| In calls | "Call sound volume" (S10) | Slider 0 to 100 %, ticks none, default 50 %, then the play button (S12) | |

Captions. Under Notification sounds: "Notification sounds use the alert volume in System Settings › Sound." with the link "Open Sound Settings". Under Conversations: "Give one conversation its own sound from its Notifications… menu." When no conversation has one, the Conversations group is absent and this caption sits under Each account instead. Under In calls: "Call sounds play on your call speaker, quieter than voices. Voice volume is in Audio & video." S12's tooltip names the device: "Play test sound on AirPods Pro".

### Appearance
| Row | Control | Second line |
|---|---|---|
| "Theme" (A1) | Absent. Search says "tinyclient is dark only for now." | The segmented control is specified for when light values are checked. It is not drawn. |
| "Text size" (A2) | Stepped slider, five stops (−1 to +3), value "13 pt" … "17 pt", "Default" at 14 | |
| "Density" (A3) | Segmented: "Comfortable" / "Compact" | Compact: "List rows are 40 instead of 48." |
| "Show message previews in the list" (A4) | Switch | Off: "Rows show names and times only." |

Caption: "Text size is also ⌘+, ⌘− and ⌘0. Reduce Motion and Increase Contrast follow System Settings." The text-size slider's ends carry an "A" glyph, 11 and 16 `text_soft`, 8 outside the track.

### Messages
| Row | Control | Second line |
|---|---|---|
| "Send with" (M1) | Segmented: "Return" / "⌘Return" | Return: "⇧Return adds a line." ⌘Return: "Return adds a line." |
| "Open conversations at" (M2) | Segmented: "First unread" / "Newest message" | |
| "Show when others are typing" (M3) | Switch | |
| "Convert emoticons to emoji" (M4) | Switch | ":) :( :D ;) <3 become 🙂 🙁 😀 😉 ❤️ when you send. Never inside code." |
| "Play GIFs and animated stickers" (M5) | Segmented: "Automatically" / "On hover" / "Never" | Never: "Click one to play it." Automatically with Reduce Motion on: "Reduce Motion is on, so they play under the pointer." |

### Privacy
| Group | Row | Control | Second line |
|---|---|---|---|
| Let others see when I am typing | One row per account (P1) | Switch; Telegram fixed "Always on"; Signal fixed "Set on your phone" | Telegram: "Telegram does not let an app turn this off." |
| Let others see what I have read | Teams accounts (P2) | Switch | "Off also leaves chats unread on your other devices." |
| Let others see what I have read | Telegram, Signal | Fixed "Always on" / "Set on your phone" | |
| | "Load images and link previews automatically" (P3) | Switch | Off: "Nothing is fetched until you click it." |
| | "Keep messages on this Mac for" (P4) | Segmented: "7 days" / "30 days" / "90 days" | |

Captions. Under the read group: "Discord and Slack have no read receipts." If no signed-in account has receipts, the group is just that caption under the label. Under P4: "Older messages are deleted here, never on the networks. Each conversation also keeps at most its newest 2,000."

**Shortening P4 asks first**, because it deletes. Modal (01, 440): "Delete messages older than 7 days?" / "41,210 messages, about 12 MB, are removed from this Mac. They stay on Slack, Discord and the others." / "Cancel" · danger "Delete". Unknown counts are dropped from the sentence, as in 08 §6. Lengthening applies without a question.

### Audio & video
| Group | Row | Control | Second line |
|---|---|---|---|
| Microphone | "Microphone" (V1) | Device picker | |
| Microphone | "Input level" (V3) and "Sensitivity" (V6), one row | Level meter, then secondary button "Test" (while on: "Stop"); under the meter, the V6 slider 0 to 100, default tick at 25, value "25" | "Below the line nothing is sent and your speaking ring stays off." While testing: "Speak normally. Green is what others would hear." |
| Speaker | "Speaker" (V2) | Device picker | |
| Speaker | "Call volume" (V5) | Slider 0 to 100 %, default 100 % | |
| Joining | "Join calls with my microphone muted" (V4) | Switch | |

The meter row is min 96 tall (8, 28, 6, 28, 2, 16, 8): "Input level" left of the meter and "Test" on the first line, "Sensitivity" left of the slider on the second, the second line under both. Without the meter (§6), Sensitivity is an ordinary slider row. **Test is a toggle, not a hold** (product said held): holding a key or button is impossible for many keyboard and switch users. A test stops on "Stop", after 30 s, when the section changes or the window closes; mediad then exits within 3 s as specified. During a call, Test uses the running mediad and starts nothing.

Caption under Speaker: "Picking a device in a call's menu changes that call only. These are the defaults." There is no Camera row. Search for camera answers "tinyclient has no camera yet." A Tiny call can still send a camera on Windows (`07`).

### Voice
Echo cancellation (V10), noise suppression (V11), and automatic gain (V12) are switches. Each defaults on. While echo cancellation is off, a Tiny channel still says to wear headphones (`07` §7a, `10`).

### Storage
| Row | Control |
|---|---|
| Usage (D1) | Block, min 72 (8, 18, 8, 8, 6, 16, 8): "496 MB on this Mac" 13/18 600 `text` with "Show in Finder" ghost button at right; 8 below, a 238 × 8 bar, radius 4, two parts, Messages `accent` and Images and files `positive`, 1 pt `raised` gap between; 6 below, a legend: 8 pt squares, "Messages 212 MB", "Images and files 284 MB", 12/16 `text_soft` tabular, 16 apart |
| "Keep images and files up to" (D2) | Segmented: "250 MB" / "500 MB" / "1 GB" / "2 GB"; second line "The oldest unused go first." |
| "Clear saved messages and files" (D3) | Danger button "Clear…", then the 08 §5 modal |

Caption: "Sign-ins, drafts and settings are not part of this."

### Shortcuts
Read-only. One card per menu (tinyclient, File, Edit, View, Conversation, Window, Help), labelled with the menu's name, then a "Calls" card for the 07 §2 call keys (⌘⇧M, ⌘⇧D, ⌘⇧H, ⌘⇧A, ⌘⇧R, ⌘⇧J). Those are in no menu today, so App Shortcuts cannot rebind them; they need a menu (Conversation or a new Call menu) before the caption below is true for them. Rows: 32 high, command 13/18 `text`, keys at the right as keycaps (3.8). The search field filters this list too. Caption at the top: "To change a shortcut, add it in System Settings › Keyboard › Keyboard Shortcuts › App Shortcuts." with the link "Open Keyboard Shortcuts". ⌘/ opens this section.

### About
As 08 §7, with "One small app for Slack, Discord, Telegram, Teams, Signal, and Messages, and for Tiny servers you run." The window still draws "One small app for Slack, Discord, Telegram, Teams and Signal." until that copy is updated. Below the acknowledgements, one card:

| Row | Control |
|---|---|
| "Copy diagnostics" (B1) | Secondary "Copy". Second line: "Version, macOS, memory and connection states. No messages, names or tokens." After a press, 4 s toast "Copied" |
| "Logs" (B2) | Secondary "Show in Finder" |
| "Reset all settings" (B3) | Danger "Reset…", modal: "Reset every setting to its default?" / "Accounts, messages and files are not touched." / "Cancel" · danger "Reset" |

## 5. Overrides from a conversation

Per-conversation settings live with the conversation and are mirrored in Sounds › Conversations.

- **Entry points.** Conversation › "Notifications…" in the menu bar (replaces "Mute…"); the same item in a row's context menu, under "Show in sidebar"; and in the details pane's menu beside "Mute". All open one popover.
- **Popover.** 01 popover, 300 wide, padding 12, anchored to the conversation header's title (menu bar, details) or to the row (context menu). Title 13/18 600 `text`, truncated: "Notifications for #design". Then two rows of 40, no card:

| Row | Control | Line below |
|---|---|---|
| "Mute" | Switch, the network's own mute (N12) | "Saved on Slack, so your other devices follow." |
| "Sound" | Sound picker, inheriting "Default (Chime)" from the account (S6) | Muted: "Muted conversations make no sound." |

  Then a hairline and a link, "Notification settings…", which opens Settings › Notifications. A network that cannot mute this conversation has no Mute row, and the line reads "Mute this in Discord." Esc or a click outside closes; a change applies on pick.
- **Marks in the list.** Mute keeps its `bell-off`. A custom sound has no mark in the list or the header; it is visible in this popover and in Sounds.
- **Search jump.** Opening a row from Accounts' menu, a search result or this popover's link puts that row on a `selected` fill for 1.5 s, then removes it in one frame, and scrolls it to 80 below the strip.

## 6. Empty, disabled and error states

| State | Where | What shows |
|---|---|---|
| Notification permission denied | Notifications, Sounds | Banner, `circle-alert` `warning`: "Notifications are turned off for tinyclient in System Settings." · "Open System Settings". Rows stay live; their values apply once it is allowed |
| Permission not asked yet | Notifications, Sounds | Banner, `bell` `text_soft`: "tinyclient hasn't asked to send notifications yet." · "Allow" (the system prompt) |
| Notifications from chatd not built (product gate) | | Notifications and Sounds' notification groups are absent; Sounds keeps In calls. Nothing says "coming soon" |
| Microphone permission denied | Audio & video | Banner, `mic-off` `warning`: "tinyclient can't use the microphone. Allow it in System Settings › Privacy & Security › Microphone." · "Open System Settings". The Input level row is absent; Sensitivity stays |
| No microphone | Audio & video | Picker reads "No microphone found"; Input level row absent |
| Chosen device unplugged | Audio & video | "(not connected)" as in 3.5; the stored choice is kept |
| Network has no read receipts | Privacy | No row for it; the group caption names it |
| Network decides (Telegram typing, Signal) | Privacy | Fixed row (3.9) |
| No accounts | Notifications, Sounds, Privacy | Each-account groups absent. Privacy's per-account groups give way to one caption: "Sign in to an account to choose what it shares." |
| chatd unreachable | Every section with chatd rows | Banner, `wifi-off` `warning`: "Can't reach tinyclient's background service. These settings return when it does." · "Restart it". Rows homed in chatd are disabled with their last known value; shell rows stay live |
| A write fails | The row | Value returns to the stored one; 12/16 `danger` line in the row: "Couldn't save. Try again." Cleared by the next change |
| Same From and To | Quiet hours | 01 input error: `danger` border, "Start and end can't be the same." Not stored |
| Search, no match | Pane | 01 empty state, glyph `search`: "No settings match “proxy”." For a Later or Never term a line follows, 13 `text_soft`: "tinyclient has no proxy setting yet." |

## 7. Search

- ⌘F focuses the field; typing while the section list has focus types into it. Esc clears a query, then closes the window.
- Results replace the pane at once (no delay, no animation). The strip heading becomes "Results". Each match is a breadcrumb, 12/16 `text_faint` ("Notifications › Quiet time"), 4, then a one-row card with the live control, so a switch changes in place. 12 between results. Order as the product doc ranks them.
- ↓ from the field moves into the results; Return on a result opens its section with the row highlighted (§5). Clearing the query returns to the section that was showing.
- The selected section in the list keeps its fill during a search, so the way back is visible.

## 8. The sounds

All are generated from code by one synthesiser, so the licence is the repository's (MIT): no samples, no third-party presets, no fonts of sounds. The synthesiser is `mediad`'s cue module: sine and two-operator FM voices, ADSR envelopes, a 4-pole (two 2-pole sections) 300 Hz high-pass and 6 kHz low-pass, all deterministic; one 2-pole section let Chime's upper FM sidebands through at 2 %. `cargo run -p mediad --example sounds -- <dir>` renders the seven bundled sounds to WAV; `afconvert -f caff -d ima4 -c 1` encodes them; `packaging/sounds/render.sh` does both. The CAF files are committed in `packaging/sounds` as `tinyclient-<name>.caf`, prefixed so they cannot collide with system sounds, and go into the app's Resources when notifications post them. A macOS test (`crates/mediad/tests/sounds.rs`) renders the WAVs again and compares their SHA-256 to `packaging/sounds/SHA256SUMS`, so the files cannot drift from the code; other platforms' maths libraries may round a sample differently. Nothing is rendered at build time.

**Rate and size.** Every sound is 22.05 kHz mono IMA4, about 11.7 KB a second, because nothing is above 6 kHz. Correction to the product doc: at 44.1 kHz the 8 s of rings alone are about 187 KB, past the 160 KB budget. At 22.05 kHz, with afconvert's padding and info chunks stripped, the seven files total 114 KB.

**Message and mention sounds.** At most 0.5 s, peak −14 dBFS, 300 Hz to 6 kHz. Rendered to a −14.5 dBFS ceiling, since IMA4 lifts a peak by up to 0.4 dB, and to an average of −32 dBFS (RMS) ±2 dB, so changing sound does not change loudness. A test decodes the committed files and checks peak and band on what plays. They differ by contour, not only timbre, so each is recognisable on a laptop speaker.

| Name | Character | Recipe | Length |
|---|---|---|---|
| Pebble (default) | soft, low | One sine at E5 (659 Hz) with a −12 dB octave partial; 5 ms attack, exponential decay to −60 dB at 300 ms; pitch falls 3% | 0.35 s |
| Drop | round, falling | Sine gliding 1,200 → 620 Hz over 60 ms, then 160 ms decay | 0.25 s |
| Tap | dry, short | FM, carrier 1,000 Hz, ratio 1.41, index 2 → 0 over 30 ms; 2 ms attack, 90 ms decay | 0.12 s |
| Chime | bright, two notes | FM bell, ratio 1:3.5, index 1.5 → 0.2; G5 then C6 (784, 1,047 Hz) 90 ms apart, each decaying over 350 ms | 0.5 s |
| Glass | clear, high | Sine A6 (1,760 Hz) plus an inharmonic partial at 2.76× (4,858 Hz, −10 dB), 3 ms attack, 400 ms decay | 0.45 s |

**Rings.** At most 4 s, peak −10 dBFS (rendered to −10.5), 300 Hz to 6 kHz, not looped (product: until withdrawal is proven to cut a playing sound).

| Name | Character | Recipe |
|---|---|---|
| Soft ring (default) | two gentle rises | C5, E5, G5 (523, 659, 784 Hz) sines 120 ms apart, each 10 ms attack and 600 ms decay; the rise at 0.0 and again at 2.0 s; silence to 4.0 s |
| Classic ring | a phone, quieter | 440 + 480 Hz, amplitude-modulated at 20 Hz with 60% depth (a trill, not a buzz); bursts of 1.0 s at 0.0 and 2.0 s, 30 ms fades; 4.0 s |

**In-call cues.** Not files: generated in mediad's mix at call time, peak −20 dBFS at 100 % of S10, so a cue sits under speech. 10 ms raised-cosine fades so nothing clicks.

| Cue | Recipe | Length |
|---|---|---|
| Someone joins / leaves | Two sines, E5 → A5 (659 → 880 Hz) / A5 → E5, 90 ms each | 180 ms |
| I mute / unmute | One tick, sine 440 / 880 Hz, 60 ms decay | 60 ms |
| I deafen / undeafen | Two ticks, one after the other (60 ms each), 440 / 880 Hz | 120 ms |
| Connection lost / back | Two sines, A4 → E4 (440 → 330 Hz) / E4 → A4, 125 ms each | 250 ms |
| Test sound (S12) | The join cue | 180 ms |

Acceptance for the set, by unit test on the WAVs: lengths and peaks within the limits; under 1% of energy below 300 Hz or above 6 kHz; total CAF size under 160 KB.

## 9. Motion

Only what 01 allows: menus and popovers in 120 ms (opacity, 4 toward the anchor), out 80; modals and the scrim in 160 (opacity, 8 rise), out 100; toasts likewise; the scrollbar fade. The window's own opening is the system's. Nothing else moves: section changes, search results, switches, segments, the reset button appearing, the search-jump highlight (on, and 1.5 s later off, each in one frame), the play glyph swap, and the meter, which steps at 10 Hz with no tweening. Reduce Motion sets every duration to 0. Nothing animates while the window is idle.

## 10. Light and dark

Every color here is a token from 01 §2, which has both values; the Settings window takes the main window's appearance in the same frame. Until A1 ships (product gate: the 23 hard-coded colors routed through tokens, light contrast checked), the Theme row is absent and both windows force dark aqua (polish 01 §6). Specific to this screen:

- Cards are `raised` on `canvas`: #1F2023 on #18191C in dark, #F4F4F6 on #FFFFFF in light. Light cards also get the 1 pt `hairline` border, because 1.07:1 is not visible alone; that is the one place a card has both, and it is per theme.
- Switch knob and slider thumb are white in both; in light they keep the 1 pt black 12% edge so they show on `switch_off`.
- Meter and bar colors use `positive` and `accent`, whose light values are already in 01.
- **Increase Contrast** (read from macOS): `hairline` → `border`, `switch_off` → `border_strong`, group captions and the section list's glyphs step `text_faint` → `text_soft`, focus rings 2 → 3.

## 11. Accessibility

The pinned GPUI exposes AccessKit roles (`role`, `aria_label`, `aria_toggled`, `aria_numeric_value`, and so on), which map to native AX roles on macOS. Every control is built through the row table in GPUI notes, so it gets its role, label and value in one place.

| Control | Role (macOS) | Label | Value |
|---|---|---|---|
| Section list | `TabList` / `Tab` | section name | selected |
| Switch | `Switch` (AXCheckBox, AXSwitch) | row label | on / off |
| Segmented | `RadioGroup` / `RadioButton` | row label / choice | selected |
| Slider | `Slider` | row label | "50 %", min, max, step |
| Dropdown, device and sound picker | `ComboBox` (AXPopUpButton) | row label | shown value, plus "default" when inherited |
| Play, reset | `Button` | "Play Pebble", "Reset Message sound to Pebble" | |
| Meter | `Meter` (AXLevelIndicator) | "Input level" | 0 to 100 |
| Time field | `TimeInput` | "Quiet hours start", "Quiet hours end" | the time |
| Search | `SearchInput` | "Search settings" | the query; result count announced |

- A row's second line and its group caption are the control's description (`aria_description`), so VoiceOver reads "Notify me about, Direct messages and mentions, pop-up button. Conversations you muted never notify."
- Keyboard: as the product doc. Tab order is search, section list (one stop; ↑ ↓ inside), then controls in reading order. Every control is reachable and operable without a pointer; no action needs a hold or a drag.
- Targets: every control is at least 28 high; reset and play buttons pad to 28 even when drawn 24.
- Contrast: every text level used here meets 01's ratios on `canvas` and `raised`; `text_faint` never sits on `overlay` or `selected`. Color is never the only signal: inherited values are grey *and* sit under a "Default (…)" menu item; the meter's line is a shape as well as the green.
- Text size applies to this window too.

## GPUI notes

- One static table, `ROWS: &[Row]`, describes every row: id, section, group, label, keywords, second-line function, control kind, home (`chatd` or `shell`), and the platforms it exists on. Rendering, search, the accessibility labels and the Shortcuts-style acceptance test all read it. A row whose feature is absent from the build is not in the table (a `cfg` or a runtime gate), which is how "absent, not greyed" is enforced.
- Values come from one `Settings` map answered by chatd (`Request::Settings`) plus `shell.prefs`, refreshed on `Change::Settings`. The window holds no copy beyond what it draws.
- Sections render as plain flex columns; the longest (Notifications with many accounts) is a few dozen rows, so no virtual list is needed. Search results are the same rows re-parented.
- Menus and popovers use `menu()`, `popover()` and `placed()`; the inheriting dropdown is `menu_item_check` plus a separator. The conversation popover is placed in the main window, not the Settings window.
- The level meter is its own element that repaints at 10 Hz from `Call::level` only while a test runs; the rest of the pane does not repaint.
- The device list is read by running `mediad --devices` when Audio & video shows and each time a device menu opens. It lists through CoreAudio, opens no device and exits at once (about 150 ms, off the main thread), so the shell links no audio code. There is no live listener: a device plugged in while the menu is closed shows the next time it opens.
- S11 preview is `NSSound` on the system alert device, released when it ends. If that routing cannot be done, `ROWS` drops the play buttons and play-on-pick.
- The switch, slider, meter, time field and reset slot are new components in `components.rs`; the existing `segmented`, `button` and overlay pieces are reused unchanged.
- The modal for P4 needs a count from chatd: `Request::Prunable { days }` → messages and bytes. Without it, the modal drops the numbers.

## New tokens

| Token | Dark | Light | Use |
|---|---|---|---|
| `switch_off` (from 08, light value new) | white 16% | black 14% | Switch track off, slider track, unlit meter segment |
| `knob` | #FFFFFF | #FFFFFF, 0 1 2 0 black 20% | Switch knob, slider thumb |

New Lucide glyphs: `sun-moon`, `hard-drive`, `command`, `play`, `rotate-ccw`, at 14 and 16. Seven sound files, 114 KB.

## In this build (2026-10-14)

Section order is Accounts, General, Notifications, Sounds, Appearance, Messages, Privacy, Audio & video, Voice, Storage, Shortcuts, About. ⌘1 through ⌘9 reach Accounts through Voice. Storage, Shortcuts, and About have no digit. The Theme row is absent. There is no Camera row. The Accounts row's ⋯ menu, N3's per-account rows, the time field, and the conversation notification popover (§5) are still absent. The pieces live in `crates/tinyclient/src/gui/settings/`: `rows.rs` (the row table, search, Shortcuts), `controls.rs` (card, row, switch, slider, meter, reset slot), `pages.rs` (sections, results, modals, device menus). Device pickers are as wide as their words, 120 to 240, and say "System default" alone; the menu, up to 440 wide, names the default device and takes ↑ ↓ Return Space and letters. The section list is one Tab stop. Two-line rows grow 2 a text step.

## Deliberately absent

- Disabled rows for unbuilt features, "coming soon", "beta" or "experimental" labels.
- A key editor (Shortcuts is read-only; the recorder in 3.8 exists only for V8, Later).
- A notification-volume slider on macOS. Windows and Linux have "Notification volume" (SV). macOS uses the alert volume, and the row is absent there.
- Previewing sounds while moving through an open menu; any looping preview; a waveform.
- Per-row "changed" dots or accent marks; color for inherited values beyond grey.
- Animation of switches, sliders, the meter or section changes; a sliding sidebar of sections.
- A preview window for appearance: the main window is the preview, and it changes in the same frame.
- A mark in the conversation list for a custom sound.

## Open points for the owner

1. **Toggle, not hold, for Test microphone.** Changed for keyboard and switch access, with a 30 s cap. The product check V3 reads "stop" for "release".
2. **Sound files at 22.05 kHz.** The product doc's 44.1 kHz does not fit 160 KB with 8 s of rings.
3. **Device listing in the shell.** CoreAudio property reads, no audio unit. Confirm it fits "no audio in the shell".
4. **Long choice sets as dropdowns.** N2, N4, N5, N6 and N11 are dropdowns, not the product doc's segmented controls: their labels do not fit 480 beside a label.
5. **Picking a sound plays it once,** like macOS. It rides on the S11 gate.
6. **Conversation › "Mute…" becomes "Notifications…"** in the menu bar, the row menu and the details menu.
7. **Shortening P4 asks first** and needs a small count request from chatd.
8. **Call keys have no menu.** The menu bar has no Call menu, so the call shortcuts cannot be rebound through App Shortcuts. Add them to a menu, or the Shortcuts caption has to name the exception.
