# 02 · Window layout and navigation

Requirements: `docs/product/05-navigation.md`. Mockup: `mockups/02-layout-navigation.html` (frames a–f). The shell draws five levels (network, Space, Group, conversation, sub-conversation), never networks.

## 1. Window

`| rail 72 | sidebar 316 (260–420) | conversation, flex, min 392 | right pane 340 (300–480) |`

| Region | Width | Fill | Notes |
| --- | --- | --- | --- |
| Rail | 72 fixed | `rail` | Never collapses. Traffic lights at x 10, y 20, in its top 52 px. |
| Sidebar | 316, drag 260–420 | `sidebar` | Drag the right hairline (6 px hit area); double-click resets. `cmd-\` hides it. |
| Conversation | flex, min 392 | `canvas` | The only region that gives up width. |
| Right pane | thread, search: 340, drag 300–480; members: 240 | `sidebar` | One slot; opening one replaces the other. `cmd-.` closes. |

- Each region starts with a 52 px **title strip** that drags the window. 1 px `hairline` between panes. Min window 540 × 440. Widths are remembered.
- **Narrow, by arithmetic** (frame f). (1) `W − 72 − sidebar − right < 392` (1120 at defaults): the right pane **replaces** the conversation; its strip gains a 26 px crumb `‹ #channel` that returns, as does `esc`. (2) `W − 72 − sidebar < 392` (780): the sidebar leaves the row. A rail click, or a 26 px panel button now first in the conversation strip, floats it at x 72 over the conversation with `shadow_pane` and `scrim`; it closes on pick, `esc` or a click outside.
- **Conversation strip**, left to right: [panel button] back, forward (26 px; `text_faint` when empty), avatar 30, title 15/650 over a 12 px `text_faint` line (`network icon · Space › Group · topic or presence`), then the call, then 28 px buttons: search, members (not in 1:1), details (not in 1:1). A person's presence is the dot on their avatar, green, yellow, red or a grey ring, with the word as its tooltip; it is not repeated in words. Window buttons on Windows and Linux are as tall as the strip, on its center line. A lit button gets `selected`. Clicking the subtitle reveals the conversation in the sidebar.
- **Right pane strip:** title 16/650 and count, or the search field; close button.
- Below the conversation strip, owned by other areas: call surface, messages (flex), typing line, composer. Navigation adds only the hit highlight and the jump pill (section 6).

## 2. Rail

44 × 44 buttons, radius 13, 10 px gap, glyph 22, network icon 26. Top to bottom:

1. **Inbox**, **People** (fixed).
2. 28 px hairline, then **one button per signed-in network**.
3. 28 px hairline, then **pinned Spaces**: squared avatar 40 (radius 12) with a 16 px network badge. Pin from a Space's menu or by dragging its row here; drag to reorder.
4. Fixed at the bottom: **add** (a network, or another Slack workspace) and **your avatar** (32, own presence), which opens status and Settings.

Zones 2–3 scroll together with no scrollbar; 1 and 4 never move. **Fifty Discord servers and five Slack workspaces are six buttons plus what you pinned.** Discord's icon column is fifty pictures scanned from memory; a named list reads faster, and the few servers you live in are one drag from a `cmd-digit`.

| State | Drawn as |
| --- | --- |
| Current sidebar mode | `selected` fill on exactly one rail button |
| Mentions and DMs | the unread dot, or the Inbox count, in accent; muted rows count only when they mention you |
| Unread, no mention | 8 px dot in `unread` (`text`), top right |
| Hover | `hover` fill; tooltip `Discord · 2 mentions · ⌘3`. Link trouble is words in that tooltip (`Discord · Reconnecting`). |

A network click opens its home in the sidebar; a pin opens its Space; clicking the lit item scrolls its list to the top. **The rail and sidebar never change the open conversation.**

## 3. Sidebar

Three parts, the same in every mode, 96 px above the list:
- **Title strip (52):** where you are on the left, that mode's one control on the right.
- **Jump strip (44):** a 32 px button drawn as a field, `Jump to…  ⌘K`, and a 32 px compose button (the switcher with `@`). It opens the switcher and does not filter: the switcher already filters everything, and one field should mean one thing. In People it is a real `Find people` field.
- **List:** one `uniform_list`. Rows are **48 px** where a preview earns the space (40 when Density is Compact), **28 px** (`ROW_ONE`) in trees.

| Mode | Title strip | List |
| --- | --- | --- |
| Inbox | `Inbox` · segmented `All · Unread n · Mentions n` (counts dropped under 300 px) | 48 px rows by recency; pinned first |
| Network home | network icon 20, name · `…` · link state in words when not connected | 28 px tree: `Direct messages` (six newest, then `Show 76 more`, which expands in place), then Spaces in the network's order. Discord folders and Telegram folders are Groups. |
| Space | `‹` 26, squared avatar 24, name · `…` (Mark all read, Pin to rail, Open in Discord) | 28 px tree: Groups › conversations › sub-rows, network order |
| People | `People` · 24 px toggles, one per signed-in network | Frame for the people area: 316 × (height − 96), 48 px rows (40 when compact), optional 36 px tab row |

**Network home is a tree of Spaces** (frame c). A Space row has two targets: the chevron opens it in place, the name focuses the sidebar on it (Space mode). A network with three or fewer Spaces starts open, so two Slack workspaces are both simply there; fifty servers start closed and read as a list. **Up a level:** `‹`, `cmd-up`, or the network's rail button.

| Row | Height | Anatomy (8 px inset each side; fill radius 10 or 7; 12 px indent per level) |
| --- | --- | --- |
| Conversation, Inbox | 48 | avatar 44 with network badge · title 14, presence dot 7 after it · pin 11, bell-off 11, time 11.5 · preview 12.5 `text_soft` · pill or dot. Channel: squared, `Space › #channel`. Thread: `#parent › thread`. |
| Space | 28 | chevron 12 (home only) · squared avatar 22 · name 14/650 · link word · pill or dot |
| Group header | 28 | chevron 12 · label 11.5/600 `text_faint` resting on the bottom 6 px, so there is air above it |
| Conversation, tree | 28 | glyph 16 (hash, megaphone, lock, forum, volume) or avatar 20 with presence for a DM · title 14 `text_soft` · trailing |
| Sub-row | 28 | 28 px further in · thread: corner glyph 14, title 13 · voice occupant: avatar 20, name 13, mic-off 13 |
| Action row | 28 | glyph 15 and a `text_faint` label: `Show 76 more`, `Browse channels`, `Add a workspace` |

| State | 48 px row | 28 px row |
| --- | --- | --- |
| Unread | title 650, time `accent`, 8 px `text` dot | title `text` 600, dot |
| Mention or DM | title 650, `unread_pill` with the count | title `text` 600, pill |
| Muted | title and preview `text_faint`, bell-off, no dot; a mention keeps its pill | same |
| Open / hover | `selected` / `hover` fill | same |
| Keyboard focus | 1.5 px inset `focus_ring`, over any fill | same |
| Typing (DM only) | preview becomes `typing…` in `accent` | none |
| Collapsed Group | n/a | chevron right; label turns `text` with a dot or the summed pill; the open conversation's row stays visible |

- **Inbox pins** carry a pin glyph and the last one draws a hairline under itself. No header row.
- **A Tiny server** uses the column in `10-tiny-server.md`. Text channels are hash rows above the voice rooms; a click opens the conversation. Direct messages are not rows in the server. Voice rooms still list seats and join on double-click. Several Tiny servers are one rail button; the server's own menu switches among them.
- **Voice channels** list who is in them as sub-rows (six, then `+4 more`) with a `text_faint` count. No speaking rings here. A click opens the channel's text and offers Join in the strip; it never joins, because Discord's join-on-click puts everyone in a room by accident.
- **Threads** you follow sit under their channel (five, then `3 more threads`, opening the list in the right pane). A **forum** is a row with the forum glyph; the conversation area draws its post list.

## 4. Opening things

- **Single click** opens in the conversation region. `cmd-click` / `cmd-Enter` opens without marking read. No double-click opens a second window. A Tiny voice room joins on double-click.
- **The sidebar does not follow.** Opening an Inbox mention, a switcher result or a search hit leaves the sidebar where it is; the row is selected if it is in the current list. Reveal is one click on the conversation subtitle. Otherwise triage from the Inbox drops you into a new server tree on every click. Exceptions: picking a Space in the switcher, and walking unread by keyboard.
- **Thread** opens in the right pane from a message's reply line; the channel keeps its scroll. The **member list** toggles from the conversation strip into the same slot.
- **Back / forward** replay 50 locations: sidebar mode, conversation, scroll anchor, right pane.

## 5. Quick switcher (`cmd-K`, frame d)

Sheet 600 wide (or `W − 48`), 96 px from the top, centred, radius 14, `overlay` fill, 1 px `hairline` border, `shadow_overlay`, over a flat `scrim`.
- **Input 52:** glyph 18, text 16, `esc` hint. `#`, `@` or `discord␣` becomes a 24 px chip.
- **Row 44,** radius 9, nine visible of twenty: avatar 28 with a 12 px network badge · title 14 `text_soft` with matched letters `text` 650 · context 12.5 `text_faint` (`Space › Group`, `Friend · Idle`, `Server · 14 channels`) · trailing slot.
- **Ranking is shown, not explained:** order is the rank; the trailing slot gives the reason or the consequence: mention pill, unread dot, bell-off, `Not joined`, `New message`.
- **Last row with any query:** `Search messages for “des”  ⌘⇧F`. **Footer 32:** the key hints.
- **Empty query:** 26 px labels `Mentions` then `Recent` (eight places; the first is the previous conversation, so `cmd-K, Enter` toggles between two).
- **No result:** one 44 px line `Nothing called “xyz”`, with the search row selected so Enter still works. **chatd down:** `chatd isn't running`, no rows.

## 6. Search (frame e)

Results take the right pane, so the conversation stays readable while you walk hits.
- **Entry:** `cmd-F`, `cmd-shift-F`, the strip's search button, or the switcher's last row. The field replaces the pane title: 32 px, `accent` border when focused, `↵` hint.
- **Scope strip 40:** full-width segmented `This conversation · <Space name> · Everywhere`.
- **Result row 80:** network icon 13, `Space › #conversation` 12 `text_faint`, a `cached` tag on local hits, date; then sender 650 and two clamped lines 13.5 with the match on `accent_soft`.
- **Footer 32** always says what was searched: `Local results · press ↵ to search Slack`, `Searched Slack · Northwind Labs · 23 results`, `Discord search isn't ready for this server yet`.
- **Jump:** Enter or click centres the conversation on the message, drawn with `accent_soft` fill and a 2 px `accent` left bar until you navigate, plus a 30 px `Jump to present` pill 12 px above the composer. `up` / `down` in the results jumps as it moves. `cmd-[` returns to the results.

## 7. Keyboard

| Keys | Action |
| --- | --- |
| `cmd-K` | Quick switcher |
| `cmd-1`, `cmd-2`, `cmd-3…9` | Inbox, People, then networks and pins in rail order |
| `alt-down` / `alt-up` | Open the next / previous row of the sidebar list |
| `alt-shift-down` / `-up` | Next / previous unread; past the end of a list it continues into the next Space or network with unread, and the sidebar follows |
| `cmd-[` / `cmd-]`, `cmd-up` | Back / forward; up a level |
| `cmd-F` / `cmd-shift-F` | Search here / everywhere |
| `cmd-\` / `cmd-.` | Hide or show the sidebar / close the right pane |
| `esc` | Close the top layer (switcher, floating sidebar, search, thread, members); with none: mark read, jump to newest, focus the composer |
| `shift-esc` | Mark the current Space read, after a one-line confirm in the sidebar strip |
| arrows, `Enter`, `left` / `right` | Move and open in the focused list; collapse / expand a Group or Space |
| `tab` / `shift-tab` | Next / previous region: sidebar, messages, composer, right pane (in the composer `tab` is completion; `shift-tab` leaves) |

Focus is in exactly one region and is drawn on the row or field that has it, never around a pane. Moving focus in the sidebar opens nothing; Enter does. Typing a character while focus is in the sidebar or messages moves it to the composer. The switcher traps focus.

## 8. Empty and loading states

All static, using the empty-state component centred in the list. The tree is drawn from chatd's table, so loading exists only before a first sync.

| List | Empty | Before first sync / offline |
| --- | --- | --- |
| Inbox | `No conversations yet` · `Sign in to a network to see your chats.` With a chip: `Nothing unread`, `No mentions` | `Syncing Discord…` in the strip; rows appear as they arrive |
| Network home | `No servers yet` · `Servers you join in Discord show up here.` | `Discord is offline` · `Showing what was saved at 10:42.` Saved rows stay usable. |
| Space | `No channels you can read` | `Loading Driftwood…` over six 28 px rows of `raised` bars (60/45/70/50/65/40% wide), no shimmer |
| Search | `No messages match “xyz”` · `Try Everywhere.` | footer line; cached rows stay |
| Thread pane | `No replies yet` | `Loading thread…` |
| No conversation open | `Pick a conversation` · `⌘K jumps anywhere.` | `chatd isn't running` |

## New tokens

| Token | Value | Use |
| --- | --- | --- |
| `overlay` | `#1b1c24` | Opaque fill of the switcher, menus, popovers |
| `scrim` | `#000000` at 45% | Behind the switcher and the floating sidebar |
| `focus_ring` | `accent`, 1.5 px inset | Keyboard focus on rows, fields, buttons |
| `shadow_overlay` / `shadow_pane` | `0 24 64 #000000a6` / `16 0 48 #00000099` | Switcher and menus / floating sidebar |

Sizes: `STRIP 52`, `RAIL 72`, `SIDEBAR 316`, `RIGHT 340`, `MEMBERS 240`, `CONVERSATION_MIN 392`,
`ROW 48`, `TREE_ROW 28` (`ROW_ONE`), `SWITCH_ROW 44`, `RESULT_ROW 80`, `ICON_BUTTON 30` and `26`.
New Lucide glyphs: users, plus, hash, volume-2, megaphone, lock, messages-square, corner-down-right, chevron-left/right/down, arrow-left/right, panel-left, pin, bell-off, mic-off, square-pen, x, ellipsis.

## GPUI notes

- Every navigation list is a `uniform_list`: Inbox and People (48, or 40 when compact), network home and Space (28), search results (80), rail zones 2–3 (54). Nothing here needs variable-height `list`; Group headers are 28 (`ROW_ONE`) and the Inbox header was removed for exactly that reason.
- The tree is a flat `Vec` of visible rows for one Space or one network home, rebuilt on collapse. chatd holds the rest, so fifty servers cost the shell one Space.
- The switcher is a plain flex column of at most twenty rows, queried from chatd per keystroke.
- Overlays are an opaque fill, a shadow and a flat scrim. No blur. Nothing in this area animates: collapse, pane open and the floating sidebar are instant.
- Pane resize is a 6 px drag strip over the hairline; widths clamp before layout, so the narrow rules are arithmetic on window width.

## Deliberately absent

- **A server-icon column.** Only pinned Spaces reach the rail.
- **A filter box in the sidebar.** The switcher does it across everything.
- **64 px rows inside a Space, and the Inbox "Pinned" header.** These differ from `05-navigation.md` section 1. A 100-channel server at 64 px is 6,400 px tall; a tree is for finding a name. One-line rows are 28, and one row height per list keeps it a `uniform_list`.
- **Search on the rail** (differs from section 2): it has two keys, a strip button and a switcher row.
- **Rail buttons as filters.** A network button is a place; Inbox and People filter in their own strips.
- **Hover-only row buttons, speaking rings, shimmer, slide or fade transitions.**
- **Split view, tabs, a second window, custom ordering, plain unread server channels in the Inbox.**

## To reconcile

1. `05-navigation.md` N13 says a thread pushes over below 1100 px; with these widths it is 1120.
2. The Inbox row's avatar corner holds the network badge, so presence is a 7 px dot after the title there and on the avatar in tree rows. People area: confirm or move it.
