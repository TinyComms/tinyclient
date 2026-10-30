# 07 · The window over time: header, right pane, selection, landing, restore, narrow, focus

Requirements: `product/polish/07-panes-selection.md` (R1–R19, table §1, ladder §6), ruling 2 in `00-rulings.md`; pane
contents are `06-profile-info.md`'s. Mockup: `mockups/07-panes.html` (frames A–H). Sizes in pt; tokens from
`theme.rs` / `design/01-system.md`. Where this file and `design/02-layout-navigation.md` differ, this one wins.

**Seen in the build** (scratch daemon; `--open` and `--right` work): members 240 vs profile 340, so text reflows on a
kind change; a 12-unread group opens at the bottom, divider off screen; `>` in the subtitle; a 700 window with a
thread shows no way to the sidebar; the composer wears the accent focus border permanently.

## 1. Conversation header (frame A)
The `STRIP` of `polish/01` (52, hairline below, buttons 28 four apart, 12 from the right). Left to right:

`16 [panel 28  8]  ‹ › 24+24 gap 2  12  avatar 24  8  title  8  subtitle (flex)  8  [call]  8  [⋯][search][threads][members][details]  12`

- **Title** 16/600 `text` on the strip's baseline 32, like every pane. **Subtitle on the same baseline**, 13/400
  `text_faint`, after 8: one line per strip, so a late subtitle never moves the title. It truncates first:

  | Kind | Subtitle | Icon buttons (right edge last) |
  |---|---|---|
  | DM | status text if set, else presence words ("Online", "last seen 2 h ago"), else `@handle` | search · **details `user`** |
  | Group DM | "4 people · Ines, Dita, Haruto" | search · members · details `info` |
  | Channel (Discord, Telegram) | `Space › Group · topic` | search · members · details `info` |
  | Slack channel | `Workspace · topic` (+ `Shared` tag 11/500 for Slack Connect) | search · **threads** · members · details |
  | Teams channel | `Team › · description` | search · threads · members · details |

- A conversation in no Space (reached from the Inbox) puts the network icon 14 before the title (6 gap).
- Path glyph is `›` everywhere. **Clicking the subtitle reveals the row in the sidebar**: hover `text_soft`, tooltip
  "Reveal in sidebar".
- **Buttons**: glyph 16 `text_soft`; hover `hover`, held `pressed`, **lit = `selected` + `text` glyph** = "this kind is
  in the pane now". Details sits nearest its pane. In a channel, Members and Details are two values of one switch.
- **Call / hand-off**: a secondary button 28 (phone 16 + "Call"). Hand-off reads "Huddle in Slack", "Call in Teams"
  with arrow-up-right 14 after it. In a call the button is the call area's "Leave" (`07-calls.md`), radius 6, no pill.
- **Back/forward** are `text_faint` at 35% when empty; never folded.
- **Overflow**, by conversation width `C`: ≥ 600 everything; 480–599 the call button drops its label (28 square,
  tooltip keeps the words); < 480 search and threads fold into a `⋯` 28 menu (members and details never fold). The
  subtitle goes before the title shrinks below 96. At `C` = 392 with the panel button the title gets 96.

## 2. The right pane: one frame, many kinds (frames B, C)
- **One width for every kind: 300, drag 260–420**, double-click the 6 hit strip resets, remembered. `MEMBERS_WIDTH` goes.
- **Title strip** (`STRIP`): close `x` 28 leads it, then `[‹ crumb]` title 16/600, no count beside it
  (`polish/01`). The close never trails the title: the window's own buttons end the strip at the
  right, and an `x` beside minimize reads as another window button. When the crumb is drawn it is
  the way out, so the close is not drawn twice. Hairline below; fill `sidebar`, hairline left.

  | Kind | Title | Opened by | Base or push |
  |---|---|---|---|
  | Details · person | "Profile" | DM details button; a member row; "View profile" on a card | base in a DM; push elsewhere |
  | Details · conversation | "Details" | details in a group or channel | base |
  | Members | "Members" (counts live in its section headers) | members button; Details' Members row | base, or push from Details |
  | Threads (Slack, Teams) | "Threads" | threads button; "3 more threads" in the sidebar | base |
  | Thread | "Thread"; its root message heads the body | a reply line | base, or push from Threads |
  | Search | the field replaces the title | `cmd-F` | base |

- **Crumb**: a ghost button 28, chevron-left 16 + the base's name 13/500 `text_soft` ("Members", "Thread",
  "Threads"), 8 before the title. At most 2 deep, so one crumb. It is not window history.
- **A profile from a thread** (frame C): an avatar in the thread opens the card (as built); the card's "View profile",
  or a name in the thread's strip, pushes the profile over the thread with `‹ Thread`. Closing the pane closes both.
- **Before data lands**: the frame and title are drawn in the same frame as the switch. The profile head (picture,
  name, handle, presence) comes from the row and is drawn at once; sections below become `hover` skeleton
  blocks (title 10, line 8, radius 4) in their final geometry, and only after 150 ms. Members draw from
  cache; with none, eight 36 skeleton rows (circle 24 + bar 40–70%). A Thread draws its root from cache
  at once; replies skeleton. The previous subject is **never** drawn for the new conversation, not even for a frame.

## 3. How the pane follows (frame B, as drawn states)
State = (channel switch: off | members | details, DM switch: off | on, thread memory per conversation).

| From → to | Frame 1 | Frame 2 (the next frame) |
|---|---|---|
| DM A, profile → DM B | profile A | profile B: head drawn, sections skeleton after 150 ms; width unchanged |
| DM B, profile → #general | profile B | channel switch decides: members of #general, scrolled to top; DM switch stays on |
| #general, members → DM C (DM switch off) | members | pane closed; the conversation widens by 300 + hairline (the only reflow, caused by the user) |
| #a, thread → #b | thread in #a | #b by its switch (members or nothing); thread put away |
| #b → #a | | thread returns, same root, scrolled to its bottom |
| pushed profile → anything | push | push dropped; the base follows its own row |
| search → anything | results | results stay; the picked hit stays `selected` |
| → deleted / removed | | conversation shows "You're no longer in #x" over its greyed cache; the pane closes |
A lit header button always equals the kind on screen; the pane never changes width between kinds or subjects.

## 4. Lists: press, selection, cursor (frame E)
- **Opens on mouse-down** (`polish/01` §7): `selected` and the new conversation land in the same frame; no
  `pressed` step on rows, no release, no drag. Header and pane buttons act on mouse-up, `pressed` while held.
- **Selected** = the open conversation, wherever its row is (Inbox, tree, People). Not in the list on show: no row
  lit, no scroll.
- **Keyboard cursor** on another row: the 2 inset `focus` ring (`polish/01`), radius 6, over any fill. Same row: fill
  + ring. Only after an arrow key; the first mouse-down removes it.
- **Reveal in sidebar**: switches the sidebar's mode if needed, scrolls the row to the middle, and shows it
  `selected`. Nothing flashes: the scroll and the fill are the feedback.
- **Held order** (R16): under the pointer or keyboard the Inbox does not reorder. Rows still update their text,
  time and badge in place. The new order applies 1 s after the pointer leaves, in one frame.
- **New rows above a scrolled list**: the list keeps its first visible row where it is, and a chip floats 8 below
  the list's top edge, centred: 24 high, radius 6, `overlay` + `border` + popover shadow, arrow-up 14 + "3 new"
  12/500 `text`. Click: scroll to top, chip gone. It also goes when the top is scrolled into view. Same chip
  in Members and Threads.

## 5. Landing (frame D)
- **Unread that does not fit**: the New divider (1 pt `accent` rule, "New" 11/600 `accent_text`, as `01`) sits
  one third down the message viewport. The **jump button** floats 16 from the right, 12 above the composer:
  secondary 28, radius 6, `overlay` + `border` + popover shadow, arrow-down 16 + "12 new" (or "Jump to latest"
  when nothing below is unread). Click or `esc` (ladder step 10): to the bottom, read.
- **Fits on screen**: opens at the bottom, divider in view, no button.
- **Return in this session** with nothing new: the exact anchor (message id + offset). With new messages since:
  the anchor, a fresh divider, and the button counting them.
- **Switcher, Back, alt-arrows**: the same rules (Back restores its recorded anchor). **Notification, mention,
  search hit, jump link**: the message is centred and drawn with `accent_soft` fill (no bar, so it is not read as a
  mention). The fill stays until you leave or press `esc`; it does not fade. `01` lets nothing fade, and a 1.2 s
  fade is too quick to notice after the switch. This overrides `03`'s fading jump target.

## 6. Restored launch (frame G)
- Prefs keep a **last-screen record**: sidebar mode, conversation key, title, subtitle, avatar key, Details
  switches, both widths, window frame. The first frame draws the rail, the sidebar strip, the conversation strip
  and the pane frame from it; the lists and messages come from chatd's cache in the next frames, landed by §5.
  Skeleton (static, final geometry) only after 150 ms. Threads and search are not restored.
- **Gone** (deleted, left, its account removed): the sidebar opens in the same mode; the conversation region shows
  the empty state "#trail-reports isn't here anymore" · "It was deleted or you left it." · secondary "Open Inbox".
  The pane stays closed. **Account signed out**: the conversation opens read-only from cache with the inline
  banner "Signed out of Slack · Sign in", and the composer is replaced by that line.

## 7. Narrow windows (frame F)
Defaults rail 72, sidebar 316, pane 300, conversation min 392. The order regions give way:
1. The conversation shrinks to 392. 2. **Details hides** below 1081: its switch stays on, its button draws unlit
(it shows what is on screen), and a click opens it once, replacing the conversation, with `‹ #general`. 3. Thread,
Threads, Search and a push replace the conversation with the same crumb (`esc` or the crumb returns). 4. Below
781 the sidebar leaves the row and the 28 panel button leads the strip in **every** replacing view, including the
pane's, so the floating sidebar is always one click away. Picking a row in it closes it (a thread is put away).

| Width | Drawn |
|---|---|
| 1440 | rail · sidebar 316 · conversation 751 · pane 300 |
| 1120 | rail · sidebar 316 · conversation 431 · pane 300 |
| 860 | rail · sidebar 316 · conversation 472; Details hidden; a thread replaces the conversation |
| 640 | rail · conversation 568 with panel button; the sidebar floats at x 72 over `scrim`, popover shadow |

## 8. Focus (frame H)
- **After every open the composer has focus**: the caret blinks, and the composer's border steps up to
  `border_strong`. **The composer gets no accent focus border:** it is the resting focus, and a permanent blue
  frame is noise (today's build). Every other field keeps `01`'s border and halo.
- **Rings on keyboard only**, as `polish/01` §7; a mouse-down clears every ring.
- **Escape, drawn**: each press removes exactly one visible thing, top first: autocomplete → menu/card →
  viewer → switcher → floating sidebar → edit → reply bar → pushed layer (crumb gone) → Thread/Search pane →
  mark read, jump to latest (button gone), composer focused. Details never closes on `esc`; `cmd-.` or its
  lit button closes it. Closing a layer returns focus to what had it, else the composer.

## New tokens
| Name | Value | Use |
|---|---|---|
| `PANE` / `PANE_RANGE` | 300 / 260–420 | every right-pane kind; replaces `RIGHT_WIDTH` 340 and `MEMBERS_WIDTH` 240 |
| `jump_target` | alias of `accent_soft` | the landed-on message |
| `FLOAT_CHIP` | 24 high, radius 6 | the "3 new" chip (an in-pane float, `polish/01` §4 stacking) |
| `composer_focus` | `border_strong` | the composer's focused border; an exception to `polish/01`'s field focus, for its owner to rule on |
Glyphs: `info`, `user`, `users`, `messages-square`, `search`, `panel-left`, `ellipsis`, `arrow-up`, `arrow-down`, `arrow-up-right`.

## GPUI notes
- **The switch in one frame.** `open(key)` updates the conversation, derives the pane subject from the switches,
  and resets the pane's scroll handle in one `update` with one `cx.notify()`. The pane is one element with a stable
  id; its width is read from prefs, never from the kind. The profile body is built synchronously from what the hub
  holds (row data first) and asks the rest without waiting. Never `cx.new` a pane entity that renders empty and
  fills on a later tick.
- **Rows open on `on_mouse_down(Left)`**, stopping propagation so the window drag strip does not start.
- **Scroll memory in a variable-height `list`.** Store `ListOffset { item_ix, offset_in_item }` translated to (message
  id, offset). On return, `reset(count)` then `scroll_to` the id's index: the anchor is relative to its own item, so
  wrong height estimates above it do not move what is seen. **First unread**: walk back from the divider, summing
  measured heights (estimate for unmeasured: 47 for a group start, 22 for a continuation, the reserved box for
  media) until ≥ viewport ÷ 3; anchor there. Media boxes come from metadata (`03`), so measuring does not shift.
  Leaving the bottom clears an `at_bottom` bit; at the bottom the list keeps `ListAlignment::Bottom` behaviour.
- **"n new" in a `uniform_list`**: rows are fixed height, so inserting n above adds exactly n × the row height (48, 36 or 28) to the
  offset in the same update; the chip counts n.
- **Focus-visible**: a `last_input: Keyboard | Pointer` flag on `Shell`, set in the key and mouse handlers.

## Deliberately absent
- Any pane animation: slide, crossfade, width tween. The fading jump highlight.
- A width per kind, per network or per conversation.
- Details closing on `esc`, a thread beside the wrong conversation, a profile kept beside a different DM.
- Auto-revealing the open row in the sidebar; reorder under the pointer; opening on arrow keys.
- Slack's "jump to unread" banner at the top: landing at the divider already does it.
- A second window, tabs inside the pane, a pane deeper than 2.


### Member viewport and cache

The member pane keeps 64 pages of 50 members. Cached rows display immediately.
Only visible pages are read, one local cache query at a time; the latest viewport
replaces queued ranges after one frame (16 ms). Missing pages precede refreshes
of retained rows. Replies from a different conversation are discarded. Closing the pane cancels
pending polls without discarding cached rows; reopening resumes from that cache.

Discord uses one live viewport subscription, driven by `Watch.members`. The
viewport rounds to the UI's 50-member pages and subscribes to Discord's own
100-row windows. Page queries never change that subscription or wait for the
network: `Members.pending: true` means the requested rows are not cached yet,
not an empty roster or a refusal. Pending reads do not block a later viewport.
Discord cache reads, viewport changes and viewport-bound member searches run
on their own ordered thread,
independent of history backfill sleeps and HTTP requests. Window `SYNC` and roster edits emit `Change::Members`; the GUI then reads the
cache again. A 250 ms cache poll covers missed notifications without sending
another subscription. Conversation details also handle pending previews.

The GUI does not request speculative adjacent pages, which would displace live
windows and cause subscription ping-pong. Discord's windows already hold nearby
rows. Presence for members inside subscribed windows is not requested a second
time by individual ID. Viewport watches settle separately from the slower
presence watches, normally within one frame, and while changing within 50 ms
(though an already sleeping watch dispatcher can add up to 200 ms).

Unsent gateway subscriptions coalesce by guild, preserving unsubscribe
transitions and only the newest window/presence state. Ready snapshots send
immediately; there is no artificial two-second timer or burst-credit budget.
The gateway keeps a rolling 100-frame/minute command budget, reserving 20 of
Discord's documented 120 for heartbeat/handshake traffic. Only exhaustion of
that actual budget delays queued snapshots.

Cache eviction prefers pages outside the viewport. Pending data retains cached
rows; after one second the pane explains that Discord has not sent the requested
members yet. Transient connection/time-out failures retry while wanted, with
2/4/8/16/32-second capped backoff. Other refusals offer Retry and preserve the
loaded roster. No failure retries per render, and old-conversation timers have
no effect after navigation. GUI and daemon must both be updated for the pending
cache response contract.
