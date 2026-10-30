# 06 · Live signals

What changes while you watch, and what it looks like. Requirements: `docs/product/04-live-signals.md`.
Mockup: `mockups/06-live-signals.html`. Tokens are from `theme.rs` unless listed under "New tokens".

1. **A number means "someone needs me". A dot means "there is something new". Nothing else exists.**
2. **The change is the signal.** Values swap in one frame. Nothing bounces, pulses, slides or fades.
3. **Healthy is silent.** A connected network shows no dot, no word, no color.

## 1. Signal hierarchy
Every conversation is in exactly one state. First match wins.

| # | State | When | Title | Preview | Time | Badge slot (right end of line 2) |
|---|---|---|---|---|---|---|
| 1 | Mention | unmuted channel, `mentions > 0` | 600 `text` | `text` | `accent` | pill `@3`, `accent` fill |
| 2 | Direct | unmuted DM or group DM, `unread > 0` | 600 `text` | `text` | `accent` | pill `3`, `accent` fill |
| 3 | Muted mention | muted, `mentions > 0` | 400 `text_faint` | `text_faint` | `text_faint` | pill `@1`, `quiet_fill` fill, `text_soft` text |
| 4 | Unread | unmuted channel, `unread > 0`, no mention | 600 `text` | `text_soft` | `text_soft` | dot 8 px, `accent` |
| 5 | Muted | muted, anything else | 400 `text_faint` | `text_faint` | `text_faint` | glyph `bell-off` 12 px, `text_faint` |
| 6 | Read | everything else | 400 `text` | `text_soft` | `text_faint` | empty |

- **Pill:** h 18, min-w 18, px 5, radius 9, 11 px bold, `on_accent` text, `99+` past 99. The `@` is a character in
  the same run and appears only on conversation rows. **Dot:** 8 px, centered in a fixed 18 x 18 slot.
- 1 and 2 are the same strength on purpose: both are a person waiting; the `@` tells them apart. A mention inside
  a DM is part of its count. A muted DM with unread is state 5. Only 1 and 2 notify. The open row keeps its state
  while the window is not key: it is not read yet.
- **@everyone / @here / @channel / role:** chatd decides from the network's own suppress settings. If it counts,
  it is state 1, drawn identically. If not, state 4 (or 5 when muted). There is no third look.

**Roll-up.** `N` = sum of the numbers inside (states 1, 2, 3). `D` = any state-4 conversation inside.

| Level | Number when | Dot when | Visual |
|---|---|---|---|
| Group header, collapsed (28) | `N > 0` | `N = 0` and `D` | pill h 16, min-w 16, px 4, radius 8, 10.5 px bold; dot 6 px; label `text_faint` → `text`. Expanded: nothing |
| Space row: server, workspace (64) | `N > 0` | `N = 0` and `D` | pill 18 / dot 8; title 600 when either shows. A muted Space is state 5 unless `N > 0` |
| Rail button: Inbox, network, pinned Space (44) | `N > 0` | `N = 0` and `D` | pill 18 at top −4, right −6; dot 8 px at top 0, right 0; both on a 2 px `rail` ring |
| Dock badge | `N > 0`, all networks | never | the integer; cleared at 0 |

- A number beats a dot; a level never shows both. Muted conversations never contribute a dot.
- **Quiet roll-up:** if every number inside comes from state 3, the parent pill is quiet (`quiet_fill`,
  `text_soft`). One loud child makes it loud. The Dock has no quiet form, so it counts them.
- The Inbox button counts the rows the Inbox contains, so its number equals the Dock badge. The sidebar heading
  summary (layout's element) reads `Nothing new` / `4 waiting` and counts `N` only.

## 2. Typing
**Row above the composer.** The fixed 20 px slot is always reserved, so messages never shift. Left-aligned with the
composer's text (20 + 16 px), 12 px `text_soft`, names 500. No avatars: the name is the information. Strings: one, `Ines is typing…`; two, `Ines and Tomasz are typing…`; three or more, `Several people are typing…`.

**Motion: a tiny region, at a low rate, for a bounded time.** Before the text sit three 4 px dots, 3 px apart, in
an 18 x 6 px box. One is at opacity 1.0, the others 0.35; the lit dot advances every 400 ms (2.5 changes a second,
stepped, no easing). They stop, all at 0.6, when the window is not key and after 30 s of continuous typing.
Appearing and vanishing are instant.

**Sidebar:** yes, for direct conversations only. The preview becomes `typing…` (1:1) or `Ines is typing…`
(group DM) in `accent`, static, no dots. Channel rows never change. Typing never moves a row or changes its state.

**Expiry.** chatd owns it and sends the typing set, not ticks. A typist leaves on the first of: their message
arrives; the network's cancel; the network's timeout after their last signal (Discord 10 s, Teams 8 s, Slack 5 s,
Telegram 5 s); the link drops. The shell also drops any entry older than 12 s.

## 3. Arrival: what moves and what does not
| Event | Rule |
|---|---|
| New message, Inbox | Preview, time and badge update in place. **The row moves to the top only when the pointer is not over the list and no arrow-key move happened in the last 2 s.** A held reorder is applied when the pointer leaves the list. Moves snap. |
| List scrolled away from top; Network or Space mode | Scrolled: the first visible row is the anchor and what is on screen stays put. Network and Space mode never reorder: the network's own order is fixed. |
| Count or dot changes | Swaps in place. No pop, no flash, no Dock bounce. |
| Open conversation, at bottom, window key | Appended, view stays pinned to the bottom (snap, not smooth scroll). Read. |
| Open, scrolled up more than 24 px | No scroll. The conversation view shows its pill `3 new` (messages below the viewport); click or End goes to the first unread. Not read until the bottom is reached. |
| Open, window not key | Appended under the "New" divider. The view follows the bottom only while the divider is on screen, then holds. Not read. |
| Own message | Appended at once at opacity 0.55; 1.0 on the echo. |
| Read on another device | Badge clears in place, the row does not move, its notification is withdrawn. |

## 4. Delivery and read marks
One 12 px glyph after the timestamp of **my newest message only**; older ones are implied.

| State | Mark | Networks |
|---|---|---|
| Pending | none; row at opacity 0.55 | all |
| Waiting (link down, under 60 s) | none; line under the body `Waiting for Slack…`, `text_faint` | all |
| Sent | `check`, `text_faint`; tooltip `Sent` | all |
| Delivered | `check-check`, `text_faint`; tooltip `Delivered` | Signal, Google Messages |
| Read | `check-check`, `accent`; tooltip `Read 9:41 AM`, or `Read` | Telegram, Teams, Signal, Google Messages |
| Failed | `circle-alert`, `danger`; line `Not sent · Retry · Delete · <reason>` | all |

Discord and Slack have no receipts, so a sent message there keeps the one `check`: it says the network has it. The
mark is on my newest message only while it is the newest in the conversation; a grouped row without a header carries
it at the row's right edge, on the first line (`03-conversation.md` §9).

## 5. Connection states
Rail link dot: 7 px in an 11 px `rail` ring at the icon's bottom right. Sidebar strip: 28 px, radius 8, under the
search field, in Network and Space mode for that network; 12 px text, 14 px glyph. Composer line: the existing
12 px line under the field.

| State | Rail | Sidebar strip | Composer | Action |
|---|---|---|---|---|
| Connecting (first 5 s) | icon at 0.55, no dot | none | none | none |
| Connected | **nothing** | none | none | none |
| Reconnecting | at once: hollow dot, 1.5 px `warning` border | after 5 s: `Reconnecting…`, `warning` on `warning_soft` | after 5 s: `Discord is reconnecting…`, `warning`, `wifi-off`. Return still works | none |
| Offline (60 s, Mac online) | solid `warning` dot | `Can't reach Discord` | `Can't reach Discord. Messages won't send.` | `Retry now` |
| Session expired | solid `danger` dot, icon at 0.55 | `Slack signed you out`, `danger` on `danger_soft` | replaced by a 46 px bar: `Slack signed you out. Your messages are still here.` | `Sign in again` |
| Signed out (never, or by me) | icon at 0.4, no dot, no badge | none; the list is the sign-in prompt | n/a | `Sign in` |
| Rate-limited | nothing | none | only if a send waits over 2 s: `Discord is slowing things down. Sending in a moment.`, `text_soft` | none |
| Mac offline | every network: hollow dot, `text_faint` | one strip in every mode: `No internet connection`, `text_soft` on `raised` | `No internet connection` | none; automatic |

- Mac offline replaces all per-network lines. Recovery removes the line; there is no "Back online". No countdowns:
  a ticking second is a repaint for no information. Last known counts stay visible in every state.
- Rail tooltip is the state in words: `Slack · Reconnecting`. Expired with the window closed: one notification,
  `Slack signed you out` / `Open tinyclient to sign in again.`

## 6. Notifications
Built (2026-10-28): chatd posts them (`chatd/src/notify.rs`, through `crates/notifier`). Windows shows them as toasts
under the AppUserModelID the installer's shortcut carries; Reply and Mark as read are toast buttons, the reply a text
field. Missed-call, after-reconnect and reply-failed notifications are not posted yet; Thread replies post as their
thread's conversation.
Posted for states 1 and 2 only. Never for state 3, my own messages, history at connect, or the conversation open in
the key window. Key window on another conversation: banner without sound.

| Kind | Title | Subtitle | Body | Actions |
|---|---|---|---|---|
| DM, group DM | `Ines Varga` | none; the group's name, `Lisbon trip` | message text | Reply, Mark as Read |
| Mention | `Tomasz Brandt` | `#platform · Northwind` | message text | Reply, Mark as Read |
| Thread reply | `Odile Ferrand` | `Thread in #platform · Northwind` | message text | Reply, Mark as Read |
| Missed call | `Ines Varga` | none | `Missed voice call` / `Missed video call` | Call Back, Message |
| After reconnect | newest sender | as above | `12 new messages` | Mark as Read |
| Reply failed | `Not sent to Ines Varga` | none | my text | click opens the draft |

- **Grouping:** one delivered notification per conversation (`threadIdentifier` = network + conversation). A newer
  message replaces it; the body is the newest three messages, one per line, prefixed `Name: ` when senders differ.
  Reply is an inline field, placeholder `Reply to Ines`, button `Send`.
- **Privacy:** every category sets the hidden-preview placeholder, so the system's "Show previews: when unlocked"
  hides everything on a locked Mac and shows `New message`, `New mention` or `Missed call`. Our setting
  `Show message text` (default on), when off, does the same always, with the network as title: `Slack` / `New mention`.
- **Sound:** yes, one, the system default. None for a replacement within 10 s, during a call, or while paused.
- **Paused** (1 h / until tomorrow): numbers and the Dock badge keep counting, nothing is posted. The rail bell
  shows `bell-off` in `warning`; tooltip `Notifications paused until 3:00 PM`.

## 7. Toasts
A toast reports **the result of something I just did that is not visible where I am looking**. If the result is on
screen (a row changed, a message appeared, a strip is showing), there is no toast.

| Toast | Glyph | Duration |
|---|---|---|
| `Copied` (link, message ID, code block; not cmd-C of a selection) | `check` | 2 s |
| `Saved to Downloads` · `Show` | `download` | 5 s |
| `Marked 14 conversations read` · `Undo` | `check-check` | 5 s |
| `Not sent to Ines Varga` · `Open` (only when that conversation is not open) | `circle-alert`, `danger` | 6 s |
| `Slack signed you out` · `Sign in again` (only when Slack is not on screen) | `log-out`, `danger` | 8 s |
| `Couldn't open Teams` (handoff to the official app failed) | `circle-alert`, `danger` | 4 s |

- **Position:** top center of the conversation pane, 12 px under the 52 px header. It never covers the composer,
  the typing row or the newest message.
- **Shape:** h 36, px 12, radius 10, `toast` fill, 1 px `hairline` border, shadow `0 8px 24px shadow`, 13 px `text`,
  14 px glyph in `text_soft`, gap 8; the action is 600 `accent` after a 1 px `hairline` rule.
- **One at a time.** A new toast replaces the current one; an identical one restarts its timer. Hover holds it.
  Window not key: no toast; if it matters (send failed, signed out), it is a notification instead.

## 8. Dock badge and menu bar
- **Dock badge** = `N` over all networks. No dot for plain unread: the API takes numbers only, and "any unread
  channel" would never reach zero. Set by chatd, so it is right with the window closed.
- **A menu-bar item exists**, on by default, hideable in Settings. It answers "is it still running, and how do I
  stop it". Hosted by the always-running process, not the shell.
  - Icon: one static 16 px template glyph (`message-circle`): plain; with a 6 px dot when `N > 0`; with a slash
    when a network is Offline or Expired. No number.
  - Menu: `Open tinyclient` · one disabled line per network (`Slack · Connected`, `Discord · Reconnecting`) ·
    `Pause Notifications` ▸ `For 1 Hour` / `Until Tomorrow` / `Resume` · `Settings…` · `Quit tinyclient Completely`.
- **cmd-Q** closes the window and keeps receiving. **opt-cmd-Q** stops chatd too. The first cmd-Q shows one alert,
  once: `tinyclient keeps receiving messages` / `Notifications still arrive while the window is closed. To stop
  completely, choose Quit tinyclient Completely from the menu bar.` Buttons `OK`, `Quit Completely`.

## New tokens
- `quiet_fill` `rgba(0xffffff1f)`: muted-mention pill and quiet roll-up pill; visible on `selected` too.
- `warning_soft` `rgba(0xe3a94a24)`: reconnecting and offline strip.
- `toast` `rgb(0x23252e)`: toast surface, opaque because there is no blur. `shadow` `rgba(0x00000066)`: its shadow.

Rule change to `docs/design.md`: `danger` is no longer only "leave a call". It also marks the two things that need
me to act, a failed send and an expired session. `warning` is transient link trouble.
New Lucide glyphs: `bell-off`, `check`, `check-check`, `circle-alert`, `download`, `log-out`.

## GPUI notes
- **Typing dots** are their own `Entity`. A 400 ms timer changes one `u8` and calls `cx.notify()` on that entity
  only: 2.5 frames a second for an 18 x 6 px change. Do not use `with_animation`; it asks for a frame at display
  rate. The timer exists only while the set is non-empty, the window is key, and under 30 s have passed.
- The typing set travels beside the snapshot and must not bump the list `version`. A badge change is one `notify`
  and one frame. Every state fits the 64 px `uniform_list` row.
- The 5 s and 60 s thresholds and toast dismissal are one-shot timers, not polls. Held reorder: keep the drawn
  order as view state and re-sort in `on_hover(false)`. Hollow dot: a 7 px div, 1.5 px border, no fill.

## Deliberately absent
- Risk or "unofficial client" wording anywhere in the client (README only). A green "connected" dot, "Back online",
  countdowns, spinners, shimmer. Any animation of badges, rows or toasts; Dock bounce; in-app sounds.
- Counts for plain channel unread; a red badge; per-network badge colors; a number in the menu bar.
- Typing in channel rows or the header; avatars in the typing row. Read marks on Discord and Slack, in groups, or on
  every message. Avatars and custom sounds in notifications; time-sensitive breakthrough; keyword alerts.
