# Polish design 03 · The Telegram home and the Teams home

Requirements: `docs/product/polish/03-telegram.md`, `04-teams.md`, `00-rulings.md`. Mockup: `mockups/03-telegram-teams.html`,
frames A–K. Built on `docs/design/01-system.md` and `polish/01-system.md` (calm rules C1–C13, row family, badges, dates),
which win where they speak. Points throughout. Window 1120 × 720: rail 72, sidebar 316.

**The idea.** Each home is *one list of conversations* in the shared row family: 48 Conversation rows for faces with last
lines, 28 Place rows for things in a tree and for folds. The homes differ only where the networks do. **Telegram's folders
are filters over one list, so they are tabs. Teams' sections are where a chat lives, so they are headers.** Neither home
invents a Space (03 §2.1, 04 §2.1).

## 1. Shared frame
- **Title strip 52:** network icon 20, title 16/600, then 28 icon buttons on the right (Telegram: `users` for Contacts,
  `ellipsis`; Teams: `ellipsis`). Trouble is the system banner at the top of the list, never a word in the strip.
- **Jump strip 44** as built. Telegram adds **folder tabs 32**, so its header is 128 (12 rows at 720); Teams' is 96.
- **Conversation row 48:** inset 8, radius 6, padding 8, avatar 32, gap 10. Line 1: title 13/18, at most one mark, time
  11 on the right. Line 2: preview 12/16, one badge on the right. Spacing 7/18/16/7. Time and badge never shrink.
- **Place row 28** (folds, teams, channels, actions): glyph 16 or squared avatar 20, gap 8, 13/18. **Section header 28**
  (first 24): 11/500 `text_faint`. Space separates groups (C8): no rules or hairlines inside a list.
- **States** follow the system: rest, hover, selected; unread is title 600 + preview `text` + one badge. Muted is every
  text `text_faint` at 400, with no bell and no count; a mention keeps its accent pill. Focus is a 2 inset ring. Line 2,
  first match wins: failed (`circle-alert` 12 + "Not sent" `danger`) > typing (`accent_text` "typing…" / "Mara is
  typing…", static) > draft (`pencil` 12 + "Draft:" `text` 500 + draft `text_soft`) > preview.
- **Badges** (C6): a mention is an accent pill with the number, no `@`. Messages to me in a person, group, bot or forum
  chat are an unfilled count (`text` on unread). Ambient unread is an 8 dot: channels, folds, collapsed teams, and Teams'
  boolean unread.
- **Dates** follow `crates/tinyclient/src/gui/time.rs` (the table at the top of that file, which cites `polish/01` §2). Today is the clock (`10:37 PM` or `22:37`). Yesterday is `Yesterday`. Two to six days is the weekday (`Mon`) on a row and the long weekday on a divider. This year is `Oct 2` on a row and `Friday, October 2` on a divider. Older is `10/2/25` on a row (day-first where the locale says so) and `October 2, 2025` on a divider. Tooltips use the long form (`Tuesday, October 13, 2026 at 10:37 PM`). No relative times. The thread reply line in `conversation/draw.rs` still says "Last reply just now" / "Last reply {ago} ago"; that line is unfinished.

## 2. Telegram home (frames A, B, D)
- **Order:** a `Pinned` header over Telegram's pins in Telegram's order, then `Chats` by recency. In a folder, the
  folder's own pins come first. No pin glyph (C4). Reading never moves a row (I8 hold).
- **Folder tabs:** system `Tabs` (13/500 `text_soft`, padding 0 8, 4 apart; selected `text` with a 2 pt bar).
  - The count is Telegram's: unread unmuted *chats*, unfilled; an accent pill when a mention is inside; none at 0.
    `All` has no count, because the rail holds it.
  - Overflow: the strip scrolls sideways and keeps the selection in view. A 28 `chevron-down` on an opaque `sidebar`
    patch opens every folder with its count, plus "Edit folders in Telegram".
  - Right-click a tab: Mark all read · Pin to rail · Open in Telegram. The choice is remembered per account; with no
    folders there is no strip.
- **Folder on the rail (P2):** a 40 tile, radius 10, `raised`, holding the folder's emoji or `folder` 20, with a 16
  Telegram badge as pinned Spaces have. A click opens the home on that tab.
- **Archive row (28),** first in `All`, absent when empty: `archive` 16 · "Archive" 13/500 `text_soft` · three newest
  names 12 `text_faint` · a dot (an accent pill for a mention, which also lights the rail) · `chevron-right` 12. It drills
  in to `‹ Archive` with no tabs; back with `‹`, `cmd-up` or the rail button.
- **Kinds:** shape says person (circle) or place (rounded square, r 8). One mark at most, after the title.

| Kind | Avatar 32 | Mark | Line 2 / rules |
|---|---|---|---|
| Person | circle; online dot 10 *only* when online | `badge-check` 14 `text_soft` if verified | text; ticks on mine |
| Bot | circle, never a dot | tag `bot` | text; no Call |
| Group | rounded square | — | `Sender: ` 500 + text; ticks on mine |
| Channel | rounded square; no photo → `megaphone` on `raised` | — | no sender; dot for unread; read-only |
| Forum | rounded square | `messages-square` 14 `text_faint` | `Topic › Sender: text`; drills in |
| Saved Messages | circle `raised`, `bookmark` 16 | — | pinned by default; no presence, ticks or Call |
| Telegram 777000 | Telegram icon 32 | `badge-check` | codes in full; read-only, no Call, never muted |
| Scam / Fake | as its kind | danger tag `Scam` / `Fake`, always | outranks verified |

- **Ticks** lead line 2, so the right side keeps two marks (C3): `check` (sent, `text_faint`) or `check-check` (read,
  `text_soft`) 14, then 4; pending is `clock` 12. Only on my last message, in person and group chats.
- **Unread reaction (P2):** `heart` 10 `accent_text` in a 16 `accent_soft` disc, in the badge slot only when it is empty.
- **Tags:** 16 high, radius 4, padding 0 4, 11/500. Neutral (`quiet_fill` + `text_soft`): `bot`, `App`, `External`,
  `Guest`. Danger (`danger_soft` + `danger` 600): `Scam`, `Fake`.
- **Never in rows:** phone numbers, member or subscriber counts, views, folder names, last-seen words. "last seen
  recently" lives in the header subtitle, Contacts and the profile.

## 3. Forum topics (C) and Contacts (I)
- **A forum** drills the sidebar: `‹` · squared avatar 24 · name 16/600 · `ellipsis`, no tabs. The conversation opens the
  newest unread topic, else General.
- **Topic row 48:** General is `hash` 16 on a `raised` circle. Others are a 24 bubble in the topic colour (`topic_tint`)
  with the first letter 11/600, or the topic's emoji as a still. Closed: `lock` 12 after the title. Line 2 is `Sender:
  text`. Order: pinned topics, then activity.
- **Contacts** (`users`): strip `‹ Contacts`, field `Find contacts`. Online first, then last seen, in Telegram's order.
  Line 2 is Telegram's words verbatim: "online", "last seen 2:14 PM", "last seen yesterday", "last seen recently /
  within a week / within a month". Never a phone number.

## 4. Sponsored block (D)
- **Where:** at the foot of an open channel, after the newest post and above the read-only bar, only when Telegram
  returns one. Never a preview, notification, Inbox row, search hit or unread.
- **Card:** `raised`, radius 8, padding 12, max 560, on the message text column, 16 above it.
  - Label line 16: "Sponsored" 11/500 `text_faint` ("Recommended" when flagged); on the right a 24 `ellipsis` with
    About sponsored messages · Report (only if `can_report`).
  - Then the sponsor photo 32 (shape by kind) with title 14/600, and the body 14/20 with links (6 lines then "Show
    more"; an optional photo, radius 8, ≤ 320 × 200).
  - A full-width secondary button 28 labelled with Telegram's `button_text`.
  - Foot 12/16 `text_faint`: `sponsor_info` and `additional_info` verbatim (advertiser, ad ID).
- **"Why am I seeing this?"** (`text_soft`, underlined) opens a 320 popover: "Telegram shows sponsored messages in large
  public channels. They are chosen by the channel's topic, not from your data." + "Learn more" (telegram.org/ads).
- **Reported to Telegram:** *viewed* once, when half the card is on screen in a key window; *clicked* before the link
  opens (t.me links in the app, others in the browser).

## 5. Teams home (E, F)
- **Title:** "Teams" with one tenant. With two or more, the tenant's name + `chevron-down` 12, as Teams' own switcher.
  A 6 dot follows when another tenant has something (accent for a mention). The menu (240) has 28 rows: tile 20, name,
  `(guest)`, dot or pill, a check on the current one, and "Blocked" for a tenant whose policy blocks the app.
- **Sections:** `Pinned` (only if any) → `Chats` → Meetings fold → `Teams` → Archived fold.
- **Chats** are 48 rows and never collapse. Shown: unread, draft or active in 7 days, at least 8, then a 28 `Show n
  more` that expands in place. Names per 04 §3 ("Ana, Ben, +3"). Unread is a dot (Teams gives a boolean); a mention is
  a pill. An urgent or important newest unread starts line 2 with `!` 12/600 `danger`: an abnormal state (C4) that
  extends ruling 8 and is never a fill.

| Chat | Avatar 32 | Mark | Line 2 |
|---|---|---|---|
| 1:1 | circle + Teams presence (§6) | `External` / `Guest` | text; `You: ` mine |
| Group | its picture, else 2 members 22 on the diagonal, front ringed 2 | — | `Ana: text` |
| Meeting | rounded square `raised`, `calendar` 16 | — | `Ben: text`, "Meeting recording" |
| App / bot | rounded square, app icon | `App` | text; never presence |
| Notes | my picture, no presence | — | text |

- **Meetings fold (28)** closes Chats: `calendar` 16 · "Meetings" 13/500 · three subjects 12 `text_faint` · chevron. It
  holds meeting chats quiet for 14 days and expands in place, with a dot roll-up. A new message moves a chat back.
- **Team row (28):** chevron 12 · squared avatar 20 (r 5) · name 13/500 · roll-up when collapsed. Teams start
  collapsed; rule 3 keeps unread channels and the open one visible.
- **Channel row (28):** the name aligns with the team's; General first, then Teams' order. **No glyph and no `#`.**
  Private shows `lock` 12 and shared `link` 12 after the name in `text_faint` (C4: a kind the avatar cannot say); the
  host team is in the tooltip. Up to 3 thread sub-rows: `corner-down-right` 14, titled by subject or first 40 characters.
- **Archived fold (28):** `archive` · "Archived teams" · "4 teams" in faint words (it counts things, not unread). It
  expands to read-only team rows.

## 6. A channel of posts (F) and Teams presence (G)
- **Header:** squared team avatar 32 · channel 16/600 · `Team › channel` 12; an "Open in Teams" ghost button and
  search. No members button.
- **Post card:** `raised`, radius 8, padding 12 16, max 640, 12 apart, with day dividers.
  - Head: avatar 32, name 14/600, time 11. Subject 16/600, 6 below; body 14/20, 8 lines then "Show more". Reactions as
    in `03-conversation`.
  - Below a hairline inside the card, a 36 **reply line**: up to 3 avatars 20 overlapped 6 · "7 replies" 13/500 ·
    " · Last reply 10:22 AM by Chen" 12 `text_faint` · chevron. Unread replies: label `accent_text` + a 6 accent dot.
  - The post whose thread is open uses `over(hover, raised)`.
- **Chat versus channel:** a chat is a stream of message rows with the composer always open. A channel is a column of
  cards, each its own thread; replies are only in the right pane, and the composer is a secondary "Start a post" (32,
  `square-pen`) that opens it with a Subject field.
- **Presence:** shapes from `05` plus **busy** (ruling 6). Chat rows show only the corner.

| Teams says | Shape | Words |
|---|---|---|
| Available | online disc | — (note, else title) |
| Busy · In a call · In a meeting · Presenting | **busy**: solid red disc, no bar | the activity |
| Do not disturb · Focusing | dnd disc with bar | the activity |
| Away · Be right back | idle crescent | the activity |
| Offline | hollow ring | Offline |
| Out of office | the availability's shape | "Out of office · until Fri" |
| Unknown | nothing | title |

- **People rows, line 2:** activity > out-of-office line > status note > title. The header says both: `Teams · In a
  meeting · Back at 4`. The note is the person's own words, unquoted, in `text_soft`; the out-of-office line is ours,
  with the end date when Teams gives one.

## 7. Consistency
Same family as Discord and Slack: 48 rows for faces with last lines, 28 for places and folds, one badge set, ticks only
where the network has them. Folds work alike: one 28 row, three faint names, a dot roll-up. Telegram's Archive *drills
in* (a separate list in Telegram); Teams' Meetings *expand in place* (they are chats in the same list). Telegram groups
have no Space, so they are 48 rows; Discord and Slack channels are 28 rows in a Space tree.

## 8. Empty, loading, error (H, I, J, K)
| List | State | Drawn |
|---|---|---|
| Telegram | first sync | no tabs yet; "Getting your chats" 13 `text_soft`, static 48 skeletons after 150 ms |
| Telegram | 600 chats | within 30 rows of the end, 100 more are asked for; 3 skeletons + 28 row "Loading older chats · 500 of 612"; flood wait "Telegram asked to wait · 40 s" |
| Folder | empty | `folder` · "Nothing in Family" · "Chats you add to this folder in Telegram show up here."; exclude-read: `check` · "All read in Family" |
| Contacts | none | `users` · "No contacts yet" · "Type a @username to find someone." |
| Telegram | offline / signed out | banner "Telegram is offline · saved 10:42 PM", rows usable, no dots / "Signed out of Telegram" + "Sign in", rows read-only |
| Teams | tenant blocked | banner "Fabrikam's policy blocks this app" + "Open in Teams"; `shield-x` · "Fabrikam (guest) is blocked" · "Its admins allow only managed devices. Northwind still works." No retry |
| Teams | first sync / offline / expired | "Getting Northwind" + skeletons / "Teams is offline · saved 10:42 AM" / "Teams session expired" + "Sign in" |
| Teams | no chats, no teams | "No chats yet" · "Type a name to start one in Northwind."; no Teams section |
| Channel | no posts | `messages-square` · "No posts yet" · "Start a post to begin a thread." |

## New tokens
| Token | Value | Use |
|---|---|---|
| `busy` | alias of `dnd` #D13F3F, solid disc | busy family on any network (ruling 6) |
| `topic_tint(c)` | Telegram's six topic colours → `tint()` hues 215, 32, 265, 150, 325, 8 | forum topic icons |
| `tag(kind)` | 16 h, r 4, px 4, 11/500; neutral `quiet_fill`/`text_soft`; danger `danger_soft`/`danger` | bot, App, External, Guest, Scam, Fake |
| `FOLDER_TABS` 32 · `POST_MAX` 640 · `SPONSORED_MAX` 560 | sizes | |
| Lucide | archive, bookmark, badge-check, check, check-check, clock, messages-square, megaphone, calendar, link, shield-x, folder, heart, eye | |

## GPUI notes
- Home lists mix 48 and 28 rows plus 24/28 headers: `list` with known heights and a prefix-sum offset table, as People
  does; never measure. Collapse, drill-in and paging rebuild the flat `Vec`.
- Tabs: `overflow_x_scroll` + `ScrollHandle`; widths from `text_system().layout_line` decide the overflow button; the
  bar is a 2 pt child div. Busy is the online disc div in `busy`. Ticks are 14 SVGs inline before the preview.
- Paging: when the last visible index passes len − 30 and chatd has more, one `more` request; ignore while in flight.
- Sponsored view: the card's bounds from `on_prepaint` against the viewport; once per `random_id` per open. Posts vary
  in height, so they use the conversation's `list`.

## Deliberately absent
- **Telegram:** a folder column, folder editing, Stories rings, emoji status and Premium stars, pin and mute glyphs,
  counts and views in rows, coloured folder icons, hiding sponsored messages (Premium).
- **Teams:** the Unread / Chats / Channels / Meeting chats chips (the Inbox chips cover them), custom sections, an
  Activity tab (Mentions covers it), `#` on channels, Tabs / Files / Wiki, a purple out-of-office glyph, presence words
  on chat rows.
