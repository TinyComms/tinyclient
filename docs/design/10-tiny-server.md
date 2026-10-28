# 10 · Tiny server

A self-hosted Tiny server is laid out as Discord lays out a server, with voice controls as
TeamSpeak has them. Code: `crates/tinyclient/src/gui/tiny/`. Wire contract and permissions:
`docs/architecture.md` (Tiny requests) and the server-management contract. Every size and colour
is a token of `01-system.md`.

```
| rail 72 | server column (sidebar) | home or channel, flex          | members 248 |
|         | ▾ header 52             | top bar 52, hairline under it                |
|         | categories, channels,   | home: icon, name, welcome,     | search      |
|         | seats under channels    |   who is in voice, who online  | role groups |
|         | call dock (in a call)   | channel: stage of tiles,       | Offline     |
|         | user panel 52           |   call bar                     |             |
```

The server opens on its **home**, never on a form. A control the account may not use is
absent, not disabled (`tiny/model.rs` decides, and is where it is tested). On a server older
than capabilities (`GET /v1/capabilities` is 404), every check falls back to the saved role and
only what that server knows is offered: no categories, no roles, no bans, timeouts, server mute
or move; Kick removes, Roles is Owner / Admin / Member.

## 1. Server column

- **Header (the sidebar's title strip):** the server's icon 24 (squared, radius 6), its name
  16/600 and a 14 chevron; the whole is a 36 high button that lights `hover`, `selected` while
  its menu is open (the chevron becomes a close glyph). The menu, below it, 240 wide: Invite
  people · Server settings · — · Create channel · Create category · — · Copy server address ·
  — · Servers (a check per signed-in server, when there are several) · Join another server ·
  Sign out of this server… (`danger`, asks first). Only what is allowed shows; Server settings
  always shows, its pages read-only where they must be.
- **Categories** fold. Header 34 (24 for the first): a 12 chevron and the name in capitals,
  11/600 `text_faint`, `text_soft` on hover; folded, the room count follows. A `+` on hover
  creates a channel in it. Rooms in no category come first, unheaded. A server without
  categories has one header, "Voice channels".
- **Voice channel row,** 28: volume glyph 16 (`positive` when I am in it), name 13
  (`text_soft` when empty, 500 when mine), a 12 radio glyph when push to talk is required, and
  occupancy as `3 / 12` (or `3`) in a chip that turns `warning` when full. Click shows the
  channel, double-click joins, right-click: Join (or Leave call) · Edit channel (Server
  Settings › Channels, at this channel; on an older server, Rename…) · Copy channel link ·
  Delete channel… (asks first).
- **Seats** under their channel, 28, indented 24: avatar 20 in its speaking ring, name
  (`text` while speaking), `You` on mine, then glyphs, each with a tooltip: priority speaker
  (megaphone, `accent_text`), server deafened / deafened (headphones off, `danger` /
  `text_faint`), server muted (mic off, `danger`), can't speak here (mic off, `warning`), muted
  (mic off, `text_faint`). Right-click is the person menu. Video and sharing glyphs wait for
  the server to say who shares (no seat field yet).
- **Drag:** a seat onto another channel moves them (`TinyCallMove`, only where the person menu
  would offer Move to that channel); a channel onto a channel puts it above, onto a category
  header to that category's end; a category onto a category puts it above. Drop targets light
  `accent_soft`. The new order is drawn at once and sent whole (`TinyLayout`).
- **User panel,** 52 at the column's foot on `rail`, under the call dock when a call is up:
  avatar 32 with presence, my name 13/600, then a line: `Voice connected · 38 ms` (coloured by
  the link: `positive`, `warning` over 120 ms or 1.5 % loss, `danger` over 250 ms or 5 %),
  `Seated in Lounge`, `Online` or `Connecting…`. Three 32 buttons: mic, headphones (each struck
  and on `danger_soft` when off; without a call they choose what the next call starts with),
  and a cog that opens Settings › Audio & video. A click on the connection line opens a
  popover above it: channel, channel bitrate, ping, jitter, loss, sent and received kb/s, path
  (Direct UDP, Direct TCP, Relayed) and codec, read every 3 s from `CallStats` while the call is
  up; until mediad measures, it says what mediad said.

## 2. The pane

- **Top bar** 52 with a hairline under it, across the pane and the member list: back and
  forward, then for the home the server glyph, its name 16/600 and `Home`; for a channel a
  crumb to the server name, a chevron, the volume glyph and the channel name, then a hairline
  and its topic (`text_soft`, truncating), and chips: `3 of 12` (or `3 in call`, `warning`
  when full), `Push to talk` (`accent_soft`), `96 kb/s`. At the right, the member list toggle
  (28, lit while shown), clear of the window's buttons.
- **Home,** a column at most 720 wide, 32 in: icon 72 squared, name 20/600, description, then
  `11 members · 8 online · 3 in voice` 12 `text_faint`. Invite people (primary, if allowed)
  and Join <default channel>. The welcome in a card (`raised`, hairline, radius 8, 16 in) with
  a message glyph in `accent_text`. **In voice now:** a card per occupied channel: name and
  occupancy, up to eight ringed faces 28, `Mara, Robin and 2 others`, and Join (or `You're
  here`, or `Full`). **Online, not in voice:** chips of 36 with the avatar, the name in its
  role colour and `You`; right-click is the person menu.
- **Channel:** the stage holds a tile per person, 196 × 148, radius 12, `raised`, centred and
  wrapping: avatar 64, name 13/600 in the role colour, `You` chip, then the most serious state
  as a chip (`Server muted` and `Can't speak here` in `danger`, else `text_soft`), and
  `Muted for you` or the volume I set for them. A speaker's tile has a 2 `positive` edge. The
  seat glyphs repeat in the tile's corner. Right-click a tile for the person menu.
- **Call bar,** floating 20 above the bottom: a pill on `overlay` holding round 44 buttons for
  mute, deafen and devices (Settings › Audio & video), then Leave (`danger_fill`, phone-off
  glyph). Under it, while mediad cancels no echo, "Wear headphones…". Not in the call: Join
  channel (primary, 36) and which microphone and speaker it will use; a full channel says so.
  An empty channel is an empty state with the same Join.
- **Member list,** 248, `sidebar` fill, hairline at its left: a search field, then groups with
  a header `MODERATOR — 2` (a dot in the role colour): owners, then the roles shown
  separately by rank, then everyone else online, then Offline at 45 % opacity. Rows 44: avatar
  32 with presence, name 13/500 in the role colour, a crown for owners, `You`; a second line
  says where they sit, or `Timed out` in `warning`. It hides when the pane would go under 440.
  Without a live socket nobody's presence is known, and nobody is listed offline.
- **Hover card,** beside the row (280): avatar 56, name in role colour, `This is you`, email
  for those who manage people, role chips (dot and name), Joined and Seen (`Online now` or
  `Last seen 2 h ago`), the channel they are in, and a timeout's end.

## 3. Person menu

Opens on right-click wherever a person is drawn: a seat, a tile, the member list, the home.
260 wide. Head: avatar 28, name in role colour, their top role (or `You`). Then:

- Profile (a modal with the hover card's facts, wider).
- **For you** (anyone but me): volume, a slider 0 to 200 % with a tick at 100 that it snaps to
  within 4 %, its value at the right; Mute for me (check). Kept per server and person in
  `shell.prefs` (`tiny_gain`), sent as `MediaRequest::Gain` while a call is up and again when
  one starts.
- **Moderation** (each where the account outranks them and holds the bit in that room):
  Server mute and Server deafen (checks), Move to ▸ (rooms with their occupancy), Disconnect
  from call, Timeout ▸ (60 s, 5 min, 10 min, 1 h, 1 day, 1 week; Remove timeout when timed
  out).
- Roles ▸ (a check per role this account may assign, with its colour dot; on an older server
  Owner, Admin, Member), Kick… and Ban… (`danger`, each a modal).
- Copy user ID.

Submenus open beside their item on hover. Checks keep the menu open; everything else closes it.

## 4. Modals

`components::modal`, over the scrim, 440 (480 for invites): title, words, fields, Cancel and the
action last. Kick: a reason for the audit (on a newer server). Ban: reason and `1 day · 7 days ·
30 days · For good`. Delete channel and Delete category say what happens to what is inside.
Create channel (in a category when opened from one), Rename channel, Create category: one name
field, 1 to 80 characters. **Invite people:** role chips (assignable roles only), a label, Max
uses `1 · 5 · 10 · 25 · No limit`, Expires after `30 min · 1 hour · 1 day · 7 days · Never`
(Never for owners and manage_server); an older server offers Member or Admin. The result, shown
once: the four steps, the `tc1.` code in a field with Copy, where it opens and its terms
("It works 5 times, for 1 day, and will not be shown again."), Done.

## 5. Server Settings

A full-window overlay over the rail and every pane (`canvas`), Escape or the round close
button (top right, `ESC` under it) leaves. Left column 232 on `sidebar`: the server's icon and
name, then pages under group labels: **Server** Overview, Roles · **People** Members, Invites,
Bans · **Voice** Channels, Voice & media · **Security** Audit log, Sign-in, Danger zone (in
`danger`). A page shows only where `settings::Page::visible` allows. Content at most 740 wide,
title 20/600 and one sentence. A page that edits keeps a draft; while it differs, an
unsaved-changes bar floats at the foot (`overlay`, radius 10, `shadow_modal`): "You have
unsaved changes." (or the server's refusal in `danger`), Reset, Save changes.

**Overview:** icon 96 with Upload image (the system's file panel; sent at once, as Discord
does) and Remove; server name with a count of 80; description (500) and welcome message (2000)
as multi-line fields; default channel and AFK channel as pickers; the AFK timeout as `Off · 1
min · 5 min · 15 min · 30 min · 1 hour`. Read-only cards for those who may not change it. The
other pages are drawn by `settings/<page>.rs`, one function each, and say "Coming in this
branch" until they are.
