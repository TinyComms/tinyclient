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
  avatar 32 with presence, my name 13/600, then a line: while the call is up, four 10 signal
  bars and `Voice connected · 38 ms` (coloured by the link's grade, 07 §7a: `positive`,
  `warning` at Fair, `danger` at Poor); `Connecting…` and `Reconnecting…` in `warning` while it
  is; else `Seated in Lounge`, `Online` or `Connecting…` (the socket). A failed call is the
  dock's to tell, never "Voice connected". Three 32 buttons, each focusable with a ring: mic,
  headphones (each struck and on `danger_soft` when off; without a call they choose what the
  next call starts with), and a cog that opens Settings › Voice. A click on the line opens
  **Call health**, 288 wide, above it: the bars, "Call health" and the grade in words at the
  right in its colour, up to three hints (glyph by level: alert in `danger` or `warning`, info,
  check in `positive`) over a hairline, then channel, channel bitrate, ping, jitter, loss, sent
  and received kb/s, path (Direct UDP, Direct TCP, Relayed) and codec, read about once a second
  from `CallStats` while the call is up; until media flows, it says what mediad said instead.
- **Call dock** over the panel, while a call of this server is up: the compact bar of 07 §2
  (bars, `Voice connected`, `Lounge / Night Shift`, the security slot, Leave).

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
  as a chip (`Server muted` and `Can't speak here` in `danger`, `Priority speaker` in
  `accent_soft`, else `text_soft`), and `Muted for you` or the volume I set for them. A
  speaker's tile has a 2 `positive` edge and a 12 px glow of it at 25 % (`accent_text` while a
  priority speaker talks). My own tile follows my call at once (mute, deafen, and my ring lit
  exactly while I am heard), not the server's echo, and carries my signal bars, top left, which
  open Call health; anyone else's ring lights on mediad's speaking edge or the server's,
  whichever comes first. The seat glyphs repeat in the tile's corner, with `volume-x` in
  `warning` when I muted them for me. **Hover** a tile: at its foot, round 28 buttons on
  `overlay`, Volume (anyone but me: the person menu, slider first) and Focus. **Click** a
  tile (or Enter on it; tiles are in the tab order with a focus ring) to focus it: that person
  is drawn 420 × 300 with avatar 112 and the name 16/600, the rest below in a wrapping row;
  click again, or its ✕, to go back. Changing channel unfocuses. Right-click a tile for the
  person menu.
- **Banner,** over the stage, 640 at most, radius 10, while my call here is not simply up:
  `Connecting to Lounge… 0:03` (`accent_wash`, Cancel); `Connection lost. Reconnecting… 0:22
  left` (`warning_soft`, "Your seat is kept while the call finds its way back.", Leave); or
  the failure as the dock words it (`danger_soft`, Rejoin where one may come back, and ✕).
  Removed from the call, a lost connection and "In a call on another device" offer Rejoin (or
  Join here); a ban, a timeout, a deleted channel and removal from the server do not, and
  nothing on a Tiny server offers to "open in" another app.
- **Call bar,** floating 20 above the bottom: a pill on `overlay` holding round 44 buttons
  (each in the tab order with a focus ring) for mute, deafen, devices (Settings › Audio &
  video) and Call health (the signal bars), then Leave (`danger_fill`, phone-off glyph; Cancel
  while connecting). The mute button rings `positive` while I am heard. A moderator's mute or
  deafen, or a channel where I can't speak, holds its button on `danger_soft` at 55 % and says
  so in its tooltip and in a pill over the bar ("A moderator muted you here", "You can't speak
  in this channel"). **Pills** over the bar, 30 high: "You're muted · ⌘⇧M to talk"
  (`danger_fill`, 07 §2a); push to talk, when V8 or the room asks for it, `Push to talk ·
  hold F` on `overlay` (with no key: "pick a key in Settings › Voice", in `warning`, opening
  it), lit `Talking` on `positive` while the key is held and `Priority speaking` on `accent`
  for the priority key; and Call health's most serious hint, when it is `Bad`, in `warning`,
  opening Call health. Under it, only while echo cancellation is switched off in Settings ›
  Voice, "Echo cancellation is off: wear headphones…". Not in the call: Join channel
  (primary, 36) and which microphone and speaker it will use; a full channel says "Lounge is
  full · 12 of 12" and "Someone has to leave before you can join." An empty channel is an
  empty state with the same Join.
- **Moved by a moderator:** the call goes on in the new room, a toast says "Moved to Raid night
  by a moderator" wherever I am, and the dock says it for 4 s. (`call.moved` does not say who;
  when it does, the name goes in place of "a moderator".)
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
260 wide. Head: avatar 28, name in role colour, their top role (or `You`). The name opens
Profile, the same modal as a click on the row. There is no Profile row. Then:

- Change picture, for me. Message, for anyone else.
- **For you** (anyone but me): volume, a slider 0 to 200 % with a tick at 100 that it snaps to
  within 4 %, its value at the right; Mute for me, or Unmute for me once they are muted, with
  the volume icon. Sent as `CallGain` when let go: chatd keeps it per account (`gain:<user id>`)
  and applies it to every call on that account, this one at once; the menu reads chatd's kept
  values back. The item stays open and the words flip at the click.
- **Moderation** (each where the account outranks them and holds the bit in that room):
  Server mute / Server unmute (mic) and Server deafen / Server undeafen (headphones). Each stays
  open, and the words and icon flip at the click, before the server confirms. Move to ▸ (rooms
  with their occupancy), Disconnect from call, Remove timeout when they are timed out, Timeout ▸
  (60 s, 5 min, 10 min, 1 h, 1 day, 1 week; Remove timeout first when timed out).
- Roles ▸ (a check per role this account may assign, with its colour dot; the check flips at the
  click; on an older server Owner, Admin, Member), Kick… and Ban… (`danger`, each a modal).
- Copy name, when they have one. Copy user ID.

Submenus open beside their item on hover. A toggle keeps the menu open; everything else closes it.

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
min · 5 min · 15 min · 30 min · 1 hour`. Read-only cards for those who may not change it.

Every page is one function in `settings/<page>.rs`; what they share (field, card, table head and
52 rows, role chip, 28 colour swatch, check box, switch row, the allow / inherit / deny cell, a
picker's face) is `settings/kit.rs`. A list opened from a picker is a `menu` below it. A
confirmation is a tiny modal over the overlay; the dangerous ones ask for the server's name to
be typed (`Type “Night Shift” to go on.`) before their action lights. A refusal shows in the
unsaved-changes bar for a save, in the modal for a modal, else as a toast. Each page opens at
its top.

- **Roles** (manage_roles, a server with roles): a 232 column — Create role (primary, makes
  "New role" just below my rank and picks it), then the roles most senior first, each a 36 row
  with a grip (only below my rank), its colour dot, name, a lock when it is at or above me, and
  its member count; @everyone last, under a hairline. Drag a role onto another to take its
  place (`TinyRoleOrder`, drawn at once). The editor: colour dot, name and count; for a locked
  role a `warning` note; name (not for @everyone); colour as eleven swatches (none and ten) and
  a hex field; "Show separately in the member list"; then the permissions in five cards —
  General, Membership, Voice, Moderation, Advanced — each row a name, one line of what it lets
  people do, and a switch. A bit I don't hold myself is faint and can't be switched on. Delete
  role (`danger`, asks, says how many lose it). Name, colour, hoist and permissions wait for
  Save.
- **Members:** search (name or email), a role filter and `n of m`; a table — check box, avatar
  32 with presence, name in role colour (crown, `You`, `Timed out`, `Sign-in group` chips),
  email under it for those who manage people, up to two role chips and `+n`, joined, last seen
  (`Online` in `positive`), and `…` for the person menu (right-click works too). Only people I
  outrank can be ticked; the head box ticks every one listed. With some ticked, a bar on
  `accent_wash`: `n selected`, Add role ▾ and Remove role ▾ (each role with how many it would
  change), Kick (one reason for all), Clear.
- **Invites:** a note that a code is shown once; Create invite (the same modal as the header
  menu), `n invites open`, Revoke all (asks). Table: label (or "No label") and when it was last
  used, made by ("Server setup" for the owner invite printed at setup), roles, `uses / max`
  (`∞` without a limit), expires (relative), Revoke — anyone's with manage_invites, my own with
  create_invites.
- **Bans:** search; table — avatar, name and email, the reason (wrapping), banned when and by
  whom, until (`For good` or relative), Unban (asks).
- **Channels:** Create channel (in the picked one's category) and Create category; a tree —
  `NO CATEGORY` (a drop target), then each category (grip, name in capitals, room count) and its
  rooms (volume glyph, name, push-to-talk and lock glyphs when they have overrides). Drag as in
  the column (`TinyLayout`). A channel's editor: name, topic, user limit as `−  8  +` with No
  limit, bitrate as a 280 slider 16–128 in steps of 8 with a tick at 64 and words for it
  (`Clear speech`, `Voice, the default`, `Music`), Video and Push to talk required switches, then
  **Permissions:** a chip per role (a dot on those with an override here) and, for the chosen
  role, each channel permission with what someone holding only that role ends up with here
  (`Can` in `positive`, `Can't`) and a three-cell control: deny (cross, `danger_soft`),
  inherit (minus), allow (check, `positive_soft`). Editable where the account has manage_roles
  and manage_channels and the role is below its rank. Delete channel / category (asks). On a
  server older than categories, a channel has only its name. Opened from a channel's "Edit
  channel", that channel is picked.
- **Voice & media,** read-only: server health (audio up / down / off from `/health`, the address,
  tinyserver's version, who is in voice), reaching the server (UDP, else TCP, port 50000; my
  own call's path when I'm in one), each channel's limit, bitrate and talk mode, and my own
  connection's numbers while I'm in a call here.
- **Audit log:** Anyone ▾ and Everything ▾ (Members, Roles, Invites, Channels, Categories,
  Voice moderation, Server) filters; a card of rows — a 32 round glyph by kind (ban, kick,
  timeout, crown, move, mic-off, shield, user-plus, volume, server), the sentence with names
  ("Mara Okafor changed Leila Haddad's roles."; "Server setup" when the server did it), the
  reason when given, then `13 h ago · Oct 28, 2026 at 4:04 AM`. The setup's run of `bootstrap`
  rows reads as one ("The server was set up, and 3 owner invites were printed."). Load older
  entries reads the next 50 (`TinyAudit`, `before` the last row) until a short page says that's
  everything. An older server shows the rows it sent with the page, unfiltered.
- **Sign-in:** the provider (its name from `/v1/auth/config`, kept by chatd as
  `TinyCommunity::provider`), the server, how many joined by sign-in group, and a note that
  group mapping is set on the server; me — avatar, name, "Signed in through Tinyauth", email,
  member since, user ID with Copy; Sign out everywhere (`danger`, asks; `TinySignOutEverywhere`).
- **Danger zone** (owners): Transfer ownership — pick a member (not an owner), "Then stop being
  an owner myself", Transfer ownership… (typed confirmation; makes them an owner first, then
  unmakes me); Revoke every invite (typed confirmation).

`TINYCLIENT_SHOT_CALL=tiny_<state>` (`gui/call/shot.rs`) draws a made-up call in the room I
sit in, `--tiny-menu stats` opens Call health and `--tiny-focus n` focuses the nth tile.
`--tiny-settings <page>` opens a page for a screenshot, with `--tiny-pick n` (the nth role,
channel, or n members ticked), `--tiny-override <role>`, `--tiny-scroll <px>`, `--tiny-owner`
and `--tiny-confirm transfer`. The demo's Night Shift has data for every page.
