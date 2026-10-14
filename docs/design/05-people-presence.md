# 05 People and presence

Requirements: `docs/product/02-people.md`. Mockups: `mockups/05-people-presence.html`. One person-row, one avatar, one
presence glyph on all four networks. All sizes are points.

## Decisions that touch other agents
- **People lives in the sidebar**, behind a `Chats | People` switch (30 px high, in the sidebar's 52 px head). The open
  conversation stays in the main pane, so picking a person opens their thread beside the list. The rail's network buttons
  narrow People exactly as they narrow Chats. No rail button of its own is needed; if layout wants one, it selects the same
  switch.
- **The network icon leaves the avatar.** The avatar's bottom-right corner belongs to presence. The network is a 14 px icon at
  the row's trailing edge, shown only in all-network views. Inside a single-network view (rail filter on, member list, a
  server) it is not drawn at all.
- **Rows need opaque hover and selected fills**, because the presence ring repaints the row background (see New tokens).

## Avatar
| Size | Used for | Presence dot |
| --- | --- | --- |
| 16 | reply quote, typing line, read-by | none |
| 20 | mention pill, call roster strip | none |
| 24 | Cmd-K row, @-autocomplete, mutual-server icons | 8, ring 2, offset -2 |
| 32 | member row, own avatar in the rail | 10, ring 2, offset -2 |
| 36 | message row | 10, ring 3, offset -3 (drawn only in DMs' header, never per message) |
| 40 | sidebar row, People row, conversation header | 12, ring 3, offset -3 |
| 80 | profile card, empty new conversation | 18, ring 4, offset 0 |
| 112 | call: active speaker | none |

Offset is the `right` and `bottom` of the ringed dot relative to the avatar box. Two bitmaps only: the 80 px list image serves
16 to 40, the 256 px card image serves 80 and 112. The product doc's 96 pt card avatar is 80 here: with no banner, 80 leaves
room for the name without a taller card.

| Kind | Shape | Content |
| --- | --- | --- |
| Person | circle | photo, else fallback |
| Bot / app | circle | photo, else fallback. A `BOT` tag (9 px bold, 4 px radius) follows the name. Never a dot |
| Group DM | circle | the network's group icon; else two 26 px circles in the 40 box (older sender top-left, newest bottom-right with a 2 px ring). At 24 and below: one fallback tile |
| Channel | radius 0.3 x size (12 at 40) | `#` glyph on the fallback gradient; a Discord server channel shows the server icon |
| Server / workspace | radius 0.3 x size | icon, else initials |

- **Fallback: keep initials on a gradient**, with one change: the hue is hashed from `network + id`, not the name, so a rename
  or nickname does not recolor a person. Text is 0.38 x size, semibold, white; at 16 and 20 one initial only.
- **Loading, missing and failed are the same picture: the fallback.** The bitmap replaces it in one frame when decoded. No
  spinner, no fade, no skeleton, no retry mark. The box is sized before anything loads, so nothing shifts.
- Animated avatars are stills everywhere (card animation is P2 and out of this round).

## Presence
Shape carries the state; color repeats it. The dot is a child of the avatar: a ring-colored disc (dot + 2 x ring) with the
glyph centered in it.

| State | Shape | Color | Word |
| --- | --- | --- | --- |
| Online | solid disc | `positive` | Online |
| Idle / away | crescent: disc with a ring-colored disc, 62% of its size, at -8%, -8% | `warning` | Idle (Discord), Away (Slack, Teams) |
| Do not disturb | disc with a ring-colored bar, 60% wide, 2 px (3 px at 80) | `danger` | Do not disturb, Busy, Presenting, Notifications paused |
| Offline / invisible | hollow: 2 px `text_faint` border, ring-colored fill | `text_faint` | Offline, "last seen 2 h ago", Out of office |
| In a call | larger badge with a `phone` glyph in `on_accent`: 16 on a 40 avatar (ring 3, offset -5), 14 on 32 and 36, 24 on 80. At 24 it falls back to the online disc | `positive` | In a call, In a huddle, In voice |
| Unknown | **nothing drawn** | | handle, or "last seen recently" |

- Unknown covers Discord non-friends, directory search results before a reply, Telegram privacy buckets and any network that
  is offline. It never borrows the hollow ring.
- No mobile indicator: only Discord has one and it is illegible at 12 px.
- **Status line** is line 2 of the row: status emoji (12 px, Unicode as text, custom emoji as a 14 px still) then status text
  in `text_soft` 13 px, one line, truncated. No custom status: the word from the table. Unknown: the `@handle`. "In a call"
  replaces custom text while it lasts.
- In the conversation header: `[network icon 13] Network · Word · emoji text` on one line.

## People view (sidebar, 316 px)
Top to bottom: switch (52 px head), search field (36 px, margin 8 16 0), scope chips (28 px high, radius 8, margin-top 10),
list.

| Rail filter | Chips | Sections inside "All" |
| --- | --- | --- |
| All networks | Online, All, Requests n (only when n > 0) | Online n, Everyone n |
| Discord | Online, All, Pending n, Blocked | Online n, Everyone n |
| Slack, Teams, Telegram | Online, All | Online n, Everyone n |

- **Order.** Online section: every state except offline and unknown, A to Z by display name. Everyone: the rest, A to Z.
  (Story 4 wins over the product table's "Slack most recent first".) A person on two networks is two rows. Regrouping after a
  presence change is applied in a batch, at most every 2 s, and never while a row menu is open.
- **Section header**: 32 px, 11 px semibold uppercase, tracking 0.06 em, `text_faint`, "ONLINE — 7", left padding 20.
- **Row**: 56 px, inset 8 px from the sidebar edges, radius 10, padding 0 10. Avatar 40 with dot, gap 12, then two lines: name
  14 px semibold (plus `BOT` tag), status line 13 px `text_soft`. Trailing: network icon 14 px (all-network view only).
- **States**: rest (ring `sidebar`), hover (`sidebar_hover`), selected when that person's conversation is open
  (`sidebar_selected`). Blocked rows: avatar 45% opacity, name `text_faint`.
- **Click a row = Message.** Click the avatar = profile card. **Hover actions**: two 28 px buttons (radius 8, gap 4) at right
  10, top 14, on a patch of the row's own opaque color with 10 px left padding, covering the text's end so nothing reflows:
  `phone` (only where tinyclient can carry or hand off the call) and `ellipsis` (Profile, Open in network, Remove friend,
  Block). There is no separate Message button; the row is the button.
- **Request rows** (same 56 px): line 2 is "Wants to be friends" plus mutual-server count, or "Request sent Oct 9". Actions
  are always visible, not hover-only: incoming `check` (`positive_soft` fill, `positive` glyph) and `x`; outgoing `x` only.
  Accept turns the row into a friend in place; nothing is ever sent without a click.
- **Search.** Every keystroke filters the held lists locally (name and handle, case- and accent-insensitive) under "In your
  lists". After 250 ms without typing and at least 2 characters, each network in scope gets one lookup, at most 25 rows, shown
  in its own section: "Slack directory · Northwind — 3", "Teams directory", "Telegram", "Discord · members of Riverbend" (the
  server last opened; one server per query). While waiting, the section holds one line of words: "Searching Telegram for
  “mar”…". Directory rows show title or handle on line 2 and no dot until the network reports one. Up/Down move, Enter opens,
  Esc clears.
- **Empty states** (glyph `users`, headline, one line). Discord, no friends: "No friends on Discord yet / Friends you add in
  Discord appear here." Online chip, nobody on: "Nobody is online / 23 people are in All." Pending: "No pending requests".
  Slack or Teams: "No direct messages yet / Type a name to find anyone in Northwind." Telegram: "No contacts yet / Type a
  @username to find someone." Search: "No one matches “x” / Checked your lists and 3 directories."
- **Network down**: rows stay, drawn without dots, under one `warning` line, 13 px: "Discord is offline. Presence is paused."

## Starting a conversation
- **From People**: click a row. **From Cmd-K**: people are a "People" group; row is avatar 24 with dot, name, handle, network
  icon; a person with no thread says "New message" at the right in `text_faint`. **From a profile card**: Message.
- All three call `start` and open the pane at once. With no existing thread the pane has the normal header and, centered,
  avatar 80 with dot, name 18 px semibold, `[icon] @handle · Network` 13 px, then "No messages yet. Nothing is created on
  Telegram until you send one." in `text_faint`. The composer is focused. No row appears in Chats until the first send
  succeeds; a typed draft is kept per person. A Discord non-friend adds one line: "You are not friends on Discord. They may
  not receive this."

## Profile card
A popover, not a pane: 320 px wide, padding 16, radius 14, fill `popover`, 1 px `popover_border`, shadow `0 12 32
rgba(0,0,0,0.5)`, max height 480 then the body scrolls. Anchored to the clicked avatar or name, 8 px gap; Esc or a click
outside closes it.

- **Head (same on every network, about 196 px, paints from cache):** avatar 80 with dot (ring `popover`) top-left; network
  chip top-right (24 px, icon 13 + name). Name 18 px semibold; handle and pronouns 13 px `text_soft`; status line: 10 px
  presence glyph, word, emoji and text (wraps to 2 lines). Unknown reads "No status shared".
- **Actions**, 32 px high, gap 8: Message (accent fill, flex), Call (raised, flex, only where calls exist), `ellipsis` 32 x 32
  (Copy username, Remove friend, Block).
- **Body**, after a hairline, arrives in one reply and only extends the card downward: About (13 px, 4 lines then "more"),
  then 28 px label/value rows. Discord: roles as 22 px chips when opened from a server, mutual servers as up to five 24 px
  icons and "+n", "Friends since". Slack: title, local time with offset ("12:48 PM · 3 h behind you"), email, phone. Telegram:
  bio, phone, last-seen wording. Teams: title, department, email, out-of-office note.
- **Foot**: "Open in Discord" 12 px `text_soft` with `external-link`.

## Member list
Right pane, 240 px, fill `sidebar`, hairline on its left. Toggled by a `users` button (32 px) in the conversation header,
shown for channels and groups only; the choice is remembered per conversation kind. It hides itself when the window is too
narrow (layout decides the width).

- **Head** 52 px: "Members" 14 px semibold, total count `text_faint`, close `x`. Then a 32 px "Search members" field (server
  lookup, same 250 ms rule).
- **Sections** are whatever the network sends, with its counts: Discord role groups then Online and Offline; Telegram Admins,
  Members; Teams Owners, Members; Slack one "Members" section. Header row 32 px, same style as People.
- **Row** 44 px, inset 8, radius 9, padding 0 8: avatar 32 with dot, gap 10, name 14 px medium, optional status line 12 px
  `text_faint` (single-line rows center the name). Offline: avatar 45% opacity, name `text_faint`. Click opens the profile
  card to the pane's left.
- **Loaded by visible range** plus one screen of overscan; total height comes from the section counts. A row inside the range
  that has not arrived is a 32 px `raised` circle and a 104 x 10 `raised` bar, static. Closing the pane ends the subscription.

## Own status
Bottom of the rail: own avatar 32 with dot (ring `rail`) in a 48 px button, 12 px from the bottom. The dot shows the most
available state among connected networks. Click opens a 288 px popover to the right (padding 8, radius 14, same surface as the
card):

- Head: avatar 40, name, one line of truth ("Do not disturb on 3 networks").
- Status text field 36 px: emoji button, text, "clears in 1 h" (never, 30 min, 1 h, 4 h, today), clear `x`. Sent to Discord,
  Slack and Teams; Telegram has none and says so in its row.
- "All networks": four 32 px rows with the presence glyph (12 px), label, a faint note where a network differs ("Away on Slack
  and Teams", "Discord and Teams only"), check on the current one.
- "Per network": four 32 px rows: icon 16, name, current glyph and word, chevron. Each opens a 208 px submenu listing only
  that network's states. Telegram reads "Online while open" and has no submenu. A disconnected network reads "Offline" in
  `warning` and is not settable.

## New tokens
| Token | Value | Why |
| --- | --- | --- |
| `sidebar_hover`, `sidebar_selected` | `#1d1e23`, `#26272c` | opaque equals of `hover` / `selected` over `sidebar`; row fill and ring |
| `canvas_hover`, `canvas_selected` | `#212228`, `#2a2b30` | same over `canvas` |
| `popover`, `popover_border` | `#1c1d24`, `rgba(255,255,255,0.09)` | card and menus; defer to the visual system if it names one |
| `positive_soft` | `rgba(63,207,142,0.16)` | Accept button |
| `presence_online / idle / dnd / offline` | aliases of `positive`, `warning`, `danger`, `text_faint` | no new colors |
| `AVATAR_*` sizes, `presence_metrics(size)` | the size table above | one source for dot, ring, offset |

## GPUI notes
- `avatar(kind, id, name, image, size)` and `presence(state, size, ring)`; every call site passes the opaque color under it.
  Each glyph is two or three plain divs: no masks, no SVG.
- People is a `list` with two known heights (32, 56); member list the same (32, 44). Offsets are a prefix sum of section
  counts, so neither measures rows.
- A group composite costs two decodes; count it twice against the ~256 cap. Rows drop bitmaps when they leave the viewport;
  the fallback needs no bitmap.
- A presence change repaints one row. Nothing animates at rest: no pulse on dots, no shimmer.
- Hover actions are an absolutely positioned child with an opaque fill, so text layout is stable.

## Deliberately absent
Mobile indicator; activity detail ("Playing X"); role colors on names; avatar decorations, banners and profile themes; merged
identities across networks; a People button in the rail; a Message button on rows; a video-style member grid; multi-select
"new group"; add friend and block UI beyond the menu items (P2); browsing a directory A to Z without typing; spinners and
skeleton shimmer.
