# Polish 04 · The views that mix networks: Inbox, People, rail, quick switcher

Requirements: `docs/product/polish/05-unified-views.md`, settled by `00-rulings.md`. Mockup:
`mockups/04-unified.html` (frames 1–10). Built on `docs/design/01-system.md`; tokens are its names. Sizes in points. Looked at: `--shot` of the demo seed (Inbox, Unread, People, switcher empty and `ma`).

**The idea: one quiet row, and order you can trust.** Colour appears only where something is for you. Rows never move under the pointer. Every list ends instead of trailing off.

## 1. Sidebar frame (Inbox and People share it)

`title strip 52 · jump strip 44 · list`. List inset 8 from the sidebar edges; rows radius 6, padding 8.
- **Inbox title strip:** `Inbox` 16/600 at x 16; segmented `All · Unread n · Mentions n` right, 12 from the edge. Counts are 11/500 tabular numerals, 5 after the label (`text_faint`, `text` when the segment is on), not pills, and hidden at 0. Under a 300 sidebar the numerals drop. Mentions counts conversations; its tooltip says "4 conversations mention you".
- **Jump strip:** 28 field `Jump to… ⌘K` (opens the switcher) and a 28 compose button 8 to its right.
- **People:** title `People`, nothing on the right; the jump strip is a real `Find people` field, full width. No network toggles, no chips (the rail is the per-network view).

## 2. Inbox

**List model.** `Pinned` (starred) first, then `Today`, `Yesterday`, `This week` (the 5 days before yesterday), `Earlier` (to 30 days). A header with no rows is not drawn. Unread and Mentions views have **no headers**: mentions first, then newest (triage order, I7); a row read there stays until the chip changes.

**Day header, 28:** 11/500 `text_faint`, sits on the bottom 6, x 8 inside the list. **Not sticky.** The time column and the next header already say where you are, and a pinned bar is one more surface over the rows. Pinned rows carry no glyph: the header says it.

**Time column by group:** Today and Yesterday show the clock (`10:41 PM`, per system locale). This week shows the weekday (`Mon`). Earlier shows the date (`Sep 24`). Pinned uses the same rule as its age. 11/500 tabular.

### Row, 48 (avatar 32, gap 10, line 1 13/18 over line 2 12/16, 7 above and below)

| Slot | Rule |
|---|---|
| Avatar | DM: picture, presence corner. Group: its picture, else two 21 discs on the diagonal, the front one with a 2 pt ring of the row's opaque fill. Channel: **the Space's icon**, rounded square (radius 8); `#` on `raised` only if the Space has none. |
| Mark | 14 tinted color network mark, `text_faint` (`text_soft` on `selected`), 6 before the name, **only when ≥ 2 networks are signed in**. Telegram draws `icons/send-horizontal.svg`. |
| Name | 13, `text`; 600 when unread. **Never truncated before the Space.** Max 180, then one ellipsis. |
| Space | After the name, 6 gap, 13/400 `text_faint`. It truncates first. If it would show fewer than 6 characters, it is dropped: the avatar already shows the Space. Slack with 2 workspaces and Teams with 2 tenants use this slot for DMs. |
| Right of line 1 | the time. |
| Line 2 | First match: **Not sent** (alert 12 + `danger` "Not sent" + faint ` · text`) · **typing** (three 4 pt dots + "Mara is typing", `accent_text`, the `06` timer) · **draft** when nothing is unread (pencil 12, "Draft:" `text`/500, then the text) · **missed call** (phone-missed 12 + `danger` "Missed call") · **preview**. `You: ` for mine, `Sender: ` in groups and channels (first name). Attachments: glyph 12 `text_faint` + word ("Photo", "2 photos", "Voice message", "Sticker", file name). Custom emoji `:name:`. A leading mention of me is stripped. Colour: `text_soft`, or `text` when unread. |
| Badge | 16 high, end of line 2. **Mention:** `accent` fill, white numeral, no "@". **Unread count:** no fill, 1 pt `border_strong` inset ring, `text` numeral. **Starred channel, plain unread:** 8 `text` dot. **Muted:** no faint unread dot; a mention still shows its filled pill. |

Sidebar text stays on one visual line per slot. Embedded line breaks and tabs in
previews, drafts, activity/status text, and names are collapsed to spaces for
sidebar display; stored messages and drafts keep their formatting. Each title
and preview slot clips overflow and follows the current text-size step, so
resizing the sidebar cannot let a preview paint into a neighboring row.

| State | Drawn as |
|---|---|
| rest / hover | none / `hover` fill. Rows open on mouse-down; there is no pressed step (`01` §7). |
| open (selected) | `selected` fill; time and mark step up to `text_soft`; presence ring uses the opaque `#2E2F31` |
| unread | name 600, line 2 `text`, time `text_soft` |
| muted | every text `text_faint`, weight 400, no glyph. A mention keeps the accent pill. |
| keyboard cursor | 2 pt `focus` ring, separate from selection (`07` R8) |

### Apps row
- One per Inbox, **only while an app conversation is unread** (or the row is expanded). Placed by the newest app message's group, after that group's people rows.
- 48: avatar = two 21 rounded squares (radius 5) of the newest two apps on the diagonal; line 1 `Apps` + chevron-right 12 `text_faint`; line 2 "Deploybot, Ticketly, Workbot +2"; badge: 8 `text` dot. **No count**. An app message naming me is a mention, so the row gets the filled pill.
- Click toggles in place. Children are 28 rows: square 20 radius 5, 8, mark 12 (mixed only), name 13 (600 when unread), preview 12 `text_faint`, time 11, 8 dot. At most 8, then `Show 3 more`. Expanded stays expanded until you leave the Inbox, even when all are read.
- Children are listed for the same 30 days; the whole set lives in each network's home (Slack Apps group).

### End of the list
After the last row: 16 above, 12/16 `text_faint` "Older conversations are in", then one link per signed-in network (`Network::label`, underline at 35%). Each link opens that network's home. One network uses the same sentence and that network's label ("Older conversations are in Tiny server"). People ends the same way: one link per signed-in network, not a fixed list of four. 24 below.

### Held order and the open row (`06`, `07` R16)
- Pointer over the list, or a list key in the last 2 s: no row moves. Changes in place (preview, time, badge) still draw. New positions wait in a **"2 new" pill**: 24 high, radius 12, `overlay` with the popover shadow, arrow-up 12 + 12/500 `text`, centred 8 below the jump strip. Click applies and scrolls to the top. Without a click, it applies 1 s after the pointer leaves.
- On apply the scroll anchor is the first visible row's key: what you were looking at stays put.
- A new message in the **open** conversation updates its row in place. It moves to Today's top on the next apply, keeps `selected` (tracked by key), and the list never scrolls to follow it.

### States
- **One network signed in:** no marks anywhere in the Inbox, People or switcher (frame 2).
- **First sync, nothing cached:** a 28 line per network at the top, mark 14 + "Getting Slack · Harbor Labs" 12 `text_faint`; rows arrive in place under it; the line goes when that network's list lands.
- **Unread, nothing:** the `01` empty state, centred, 64 above centre: check-circle 20 `text_faint`, "You're all caught up" 16/600, "Last: Ines Marchetti, 10:09 PM" 13 `text_soft`. If muted conversations hold unread: a secondary 28 button "3 muted conversations have new messages", which lists them under a `Muted` header. **Mentions, nothing:** "No one's waiting on you" · "Mentions from the last 30 days show here."
- **All, nothing:** "No conversations yet" · "Sign in to a network to see your chats."

## 3. People: who's around now
Order: `Requests` line · `In a call` · `Online` · `Recent` · links. Each section A to Z.
- **Requests**, a 28 action row, only when there are any: user-plus 16, "2 friend requests" 13 `text`, the network mark 14 and chevron right. It opens Discord's Pending. One row per network with requests.
- **Headers** as the Inbox day header, count after the label in the same style: `Online  6`.
- **Row 48** (same frame as the Inbox row): avatar 32 + presence corner (`05` shapes; in a call is the 18 phone badge; unknown draws nothing) · mark (≥ 2 networks) · name 13 `text`. Line 2, first match: voice/huddle/call place ("Campfire · Moss & Lichen", volume or phone glyph 12) · status emoji + text · activity ("Playing Outer Ridge") · Teams "In a meeting until 11:00" · Slack/Teams title when 2 workspaces · Recent: "Last message Tue" (date rule of §2) · Telegram "Last seen recently" as words. Nothing → the name is centred alone, row stays 48. Never "Online".
- **Hover:** two 28 × 24 buttons (message, phone where the network can call) 8 from the right on a patch of the opaque hover fill, covering the line's end. Click the row = open the DM; avatar = card.
- **Selected** when that person's DM is open.
- **End:** one link per signed-in network (`Network::label`), the same sentence shape as the Inbox end, not a fixed list of four.
- **Nobody around:** in place of In a call and Online, one 28 line `text_soft`: "Nobody's around right now", then Recent.
- **Search** (frame 6): every keystroke filters your people locally (name, display name, handle, accent-folded) under `Your people n`. After **300 ms idle with ≥ 2 characters**, each signed-in network is asked once, the earlier request cancelled; each answers under its own header (`Slack · Harbor Labs`, `Teams · Northwind`, `Telegram`, `Discord · members of Moss & Lichen`) with at most 8 rows, people already shown removed. A network still asking is a 28 line "Asking Telegram for "mar"…" in `text_faint` under its header. Nothing spins, and nothing is reordered above the keyboard cursor. Matched letters are 600. Bots are dropped. No result anywhere: "No one called "xyz"" · "Checked your people and 3 networks."

## 4. Rail (72)
Order: traffic-light zone 52 · Inbox · People · 28 hairline · networks · 28 hairline · pins · (scroll ends) · add · own avatar. Buttons 44, radius 10, 8 apart; glyphs 20 Lucide, network marks 24 in full colour.
- **Selected:** `selected` fill on the 44 tile and the glyph `text` (rest `text_soft`). No bar, no accent. Checked at 2× (frame 7): #2E2F31 on #121315 reads clearly next to the 6% hover.
- **Badges.** Inbox only gets a number: DMs and groups unread + mentions, unmuted (= Dock badge). It is **blue when a mention is inside**, otherwise outlined (`sidebar` fill, 1 pt `border_strong`, `text`). At top −4 right −6, ringed 2 in `sidebar`. **Everything else gets a dot**, 8, top 0 right 0, ringed 2: `accent` for a mention inside, `text` for unread. Nothing for muted. People shows a dot only for requests.
- **Networks:** one button each, however many accounts. Tooltip: "Slack · 2 workspaces ⌘4". An account in trouble: icon at 40%, no dot of its own (the home shows the `06` banner).
- **Pins:** Space icon 36 (radius 9) centred in the 44 tile. A pinned **Telegram folder** is a `raised` 36 tile with the folder's emoji 18, else its first two letters 13/600 `text_soft`. Tooltip "Work · Telegram folder". Pins carry no network mark. Drag reorders; new pins append. No cap. The networks-and-pins zone scrolls, with no scrollbar. When it overflows, a 1 pt `hairline` across the rail marks each cut edge. A 720-tall rail shows about 4 network buttons and 3 pins before it scrolls; a fifth network is further down the same zone, not dropped. `⌘3–⌘9` follow rail order and stop at 9.
- **Add:** stays (a second account), drawn quietly as a 32 ghost button, plus 16 `text_faint`.
- **Own avatar:** 32, the first signed-in account's picture, presence dot of the "Set all" state (if accounts differ, the first account's). The menu (frame 7) shows picture, name, "Picture from Discord", `Set all to …`, one row per account (mark 16, account, its state), Settings ⌘,. Each account row's submenu has "Use this picture".

## 5. Quick switcher (`⌘K`, `⌘N` opens it with `@`)
- **Sheet:** width `min(560, W − 48)`, centred on the **window**, top 72, radius 12, `overlay`, border, modal shadow. No scrim. Height grows with content up to `min(20 rows, H − 144)`. Input 52 (search 16, text 16, `esc` keycap); body padding 4 6 6; footer 32. `SWITCHER_TOP` is 72.
- **Never blank.** Nothing typed: `Needs you` (≤ 5: mentions, then unread DMs, newest first), then `Recent` (8, last opened; first is the previous conversation with "Previous" trailing, so `⌘K ↵` toggles).
- **Row 44**, radius 6: avatar 28 (person circle with an 8 dot ringed in `overlay`; channel = the Space icon; Space = its icon) · title 13 · 8 · context 12 `text_soft` (`text_faint` is not used on `overlay`): mark 12 + `Moss & Lichen › Projects` / `Harbor Labs · Direct message` / `Friend · @marit` / `Server · 9 channels`. A DM's context names the Space only for Slack/Teams. Trailing: badge as §2, or 12 `text_soft` words: `New message` (person, no DM), `Not joined`, `Previous`.
- **With a query:** groups `Conversations` (≤ 6), `People` (≤ 4), `Spaces` (≤ 3), headers 28 like the day header in `text_soft`. Groups are ordered by their best hit, rows by rank (`02` §5). The first row is selected. Typed letters stay `text_soft`; matched letters are `text`/600. A person with a DM appears once, as the DM. The last row is always `Search messages for "mar"  ⌘⇧F`.
- **Network people-search:** the same 300 ms rule. While it runs, the People group ends with a 28 line "Asking Slack and Telegram…". Results insert **below** the selected row, never above it.
- **Keys:** ↑↓ move · `↵` open (a person without a DM: open the new DM, composer focused) · `⌘↵` open without marking read · `⌥↵` profile in the right pane · `esc` closes, focus back where it was. `#` / `@` / `slack␣` become a 24 chip. The footer names the first five.

## 6. Moving between places
| From → to | Sidebar | Open conversation | Title strip |
|---|---|---|---|
| Inbox row / People row | stays | opens that conversation; row `selected` | `Inbox` / `People` |
| Rail network | network home | unchanged; its row selected if listed | mark 20 + network name |
| Rail pin (Space) | Space tree | opens its last channel (ruling 07-2) | Space icon 24 + name |
| Rail pin (Telegram folder) | Telegram home, that folder's chip on | unchanged | Telegram |
| Switcher result | stays (a Space result goes to the Space) | opens | unchanged |
| "Older conversations" link | network home | unchanged | network |

Inbox chip and People search are kept per window session, along with each mode's scroll. Back/forward (`⌘[`/`⌘]`) step through **conversations** and restore the mode each one was opened from. A mode change alone is not an entry (`07` R7). Clicking the lit rail button scrolls to the top.

## New tokens
| Name | Value | Use |
|---|---|---|
| `count_ring` | = `border_strong` (white 20%) as a 1 pt inset ring | the unfilled unread count, rows and rail |
| `DAY_HEADER` | 28 | Inbox day headers, People sections, switcher groups |
| `APP_ROW` | 28 | Apps children, Requests line, "Asking…" and sync lines |
| `NEW_PILL` | 24 high, radius 12 | the held-order pill |
| `SWITCHER_TOP` | 72 | sheet position |
| `SEARCH_IDLE` | 300 ms, ≥ 2 chars | People and switcher network lookups |
| assets `networks/<name>-mono.svg` | not in the tree | do not load them. Mixed-list marks tint the color SVG; Telegram uses `icons/send-horizontal.svg`. |

The parallel system file may rename these. The values stand.

## GPUI notes
- The Inbox and People mix 48 rows with 28 headers and children, so they use **`list()` with `ListState`**, not `uniform_list`. The 30-day cut and the 500-row ceiling keep the item count small. Measure once per kind (`ListState::splice` on change), never per frame.
- Held order: keep the drawn `Vec<Key>` as view state; incoming data updates the row structs by key. The order is rebuilt in `on_hover(false)` + 1 s timer or on the pill click, with `ListState::scroll_to` the remembered anchor key.
- Mixed-list marks call `sidebar/chat.rs` `mark`: tint the color network SVG, except Telegram, which draws `icons/send-horizontal.svg`. Do not load `networks/slack-mono.svg`. Those silhouettes are not in the tree.
- Space truncation: lay out the name with `flex_none().max_w(180)`; give the Space `flex_shrink` and `min_w_0`. Measure the Space once per row change (cached `ShapedLine` width) to decide drop vs ellipsis. Do not run `shape_line` 3× per frame (audit `sidebar.rs:757`).
- Unfilled count: a `div` with `border_1().border_color(count_ring)` and the row's surface fill.
- Lookups: one `Task` per network held by the view and replaced on each keystroke (dropping cancels); the 300 ms delay is `cx.background_executor().timer`.
- Rail overflow hairlines: read `ScrollHandle::offset()` / `max_offset()`; draw 1 pt divs at the zone's edges when not at that end.

## Deliberately absent
- Sticky day headers; headers in Unread/Mentions; a `Pinned` star on each row.
- Per-network chips in the Inbox; network toggles in People; an everyone A–Z section; merged people.
- Full-colour logos in rows; a network mark on any avatar or pin; network marks with one network signed in.
- Counts on network buttons or pins; a rail selection bar; connection dots on the rail.
- Hover buttons on Inbox rows (right-click menu only); unread-first order in All; rows with no message.
- Spinners, shimmer and animation of rows, pills or badges; automatic scroll to follow a new message.

