# Polish 01 · Discord lists

What the Discord home, a server's tree and the friends list show, in what order, and what each row
carries. Looked at: `--seed-demo` and `--seed-demo=many` shots of `--mode discord`, `--mode space`,
`--mode people`, a DM opened with its profile, and the Inbox. Tags as in `05`: **[C]** our adapter
already parses it, **[U]** from memory of the official client, confirm on a live session.

## 1. What reads as "thrown at you" today

| # | Shot | Finding |
|---|---|---|
| F1 | discord | DMs and servers are one list of equal 28 px rows. DMs are capped at 6 (`DIRECT_SHOWN`) while servers are not: with ~20 open DMs his main list hides behind "Show 14 more" and 50 servers get all the room (`home-many`). The weights are backwards. |
| F2 | discord | Every server row has a disclosure chevron that expands its whole tree inline, so home is a tree of trees and there are two ways to open a server. |
| F3 | discord-many | Folders are thrown away: `ready.rs` flattens `guild_folders` into one order. 50 servers are one undivided column. |
| F4 | discord | A DM row is a name and a 20 px avatar: no status, no activity, no "in a call". A group DM is a generic icon with no members. |
| F5 | discord, space | Nobody in voice is visible anywhere until I open a server and scroll to the bottom of its tree, the one thing that pulls him into calls. |
| F6 | discord | No Friends entry and no request count on the Discord home; friends are only in the cross-network People view. |
| F7 | people | The status line says "Online" under a green dot under an "Online — 21" header: the same fact three times. Discord friends and Slack colleagues are interleaved. |
| F8 | space | All 40 channels have equal weight; read threads and a thread titled "Deleted message" are listed; empty voice channels look like live ones; muted rows carry a bell glyph on every row (noise: dimming already says it). |
| F9 | discord | A muted DM with a mention (`Mirela`) shows a grey `@1` and no mute glyph, while a muted DM without one shows the glyph: two mute looks. |
| F10 | space | The user's own Discord settings are ignored: "Hide muted channels", and the onboarding "Show all channels" opt-in, so a community server shows channels Discord hides from him. |

## 2. Rules: Discord home (`--mode discord`)

Top to bottom, one scroll, every section a collapsible Group with a rollup (N5):

1. **Friends** row (always): icon, "Friends", online count as quiet text, incoming request count as
   a neutral number. Opens People scoped to Discord. **Message requests** row under it, only when
   non-empty. Requests never count toward any badge, rail or dock.
2. **In voice** (only when a friend is in a voice channel or a DM call; at most 3 rows, then
   "n more"): one row per channel, "Campfire · Moss & Lichen", the friends' faces (up to 4) and the
   total count. Click opens the channel; Join stays an explicit button. My own call is the dock, not here.
3. **Direct messages**, Discord's order (newest message first). Shown: every DM with a number, then
   the newest up to **10** rows total; "Show n more" expands in place for the session. Closed DMs are
   not listed (Discord does not); they come back with a new message or via Cmd-K.
4. **Servers**, the user's own order **with folders as Groups**. A folder starts collapsed and shows
   its name (or the first three server names), its colour as a 3 px mark, and the rollup. A server
   outside a folder is a plain row. A pinned server stays in place here and is also on the rail.
5. Chips at the top, as in the Inbox: `All · Unread`. Unread keeps only rows and folders with a badge.

- **A collapsed Group never hides something that needs me.** Collapsed folder, category or DM
  section keeps: the open row, rows with a number, and (categories only) unread unmuted channels
  and voice channels with people. This is Discord's own collapsed-category rule [U], applied to
  folders too, and it makes "collapse everything" the scan mode for 50 servers.
- **No inline expansion of servers.** A server row opens its Space (the tree replaces home; back
  returns to home at the same scroll). Delete the chevron.
- **Muted is dim, not a glyph.** The bell-off glyph appears on hover and in the row's menu only.
  Muted rows keep their place and a quiet count for mentions (one look for F9).

## 3. Rules: a server's tree (`--mode space`)

- **Header**: back, server icon, name, the server menu. No banner, boost level, member count or
  online count (the count needs a subscription; the rest is decoration).
- **Under the header, at most one row**: a live or within-24-h scheduled event ("Event · Tonight
  20:00 · Spore walk"), P2; else nothing. "Channels & Roles", onboarding and server boosts are
  `Open in Discord` in the server menu.
- **Order** is Discord's (`position`, text before voice in a category) [C]. No re-sorting by unread.
- **Visibility** honors, in order: `VIEW_CHANNEL` [C]; the onboarding opt-in, showing only opted-in
  channels plus a last row "Browse n more channels" that reveals the rest from the stored tree [U:
  `user_guild_settings.flags` and per-channel override flags]; "Hide muted channels" when he set it
  in Discord [U: `hide_muted_channels`]. A forbidden channel is absent, never locked-and-shown.
- **Default collapse**: categories he muted start collapsed. In a server with over 80 visible
  channels, every category without a number starts collapsed. His own collapse/expand wins after.
- **Threads** under a channel: only threads I joined that are unread or open, at most 3; the rest are
  "n threads" opening the channel's thread list in the right pane. No "Deleted message" title: a
  thread whose starter is gone uses its own name.
- **Voice / stage**: occupied channels list occupants (cap 6, `+n`), friends first, each with mute,
  deafen and live glyphs. Empty voice channels are drawn at muted-row weight. A stage shows "Live"
  and the speaker count; joining a stage opens Discord (`06`).
- **Forum / media**: one row; opens a post list in the conversation area (N14), never posts as tree rows.
- **Announcement**: megaphone glyph. "Follow" (into another server) is cut: an admin action.
- **NSFW** (`nsfw` on the channel [U: not parsed]): row unchanged; the first open per channel shows
  a one-line gate, and no preview of it reaches the Inbox or a notification until passed.
- A 300-channel server: header, 10 to 20 category headers, the unread channels inside them, and
  the voice channels with people. Nothing else until he expands.

## 4. Rules: Friends (People scoped to Discord)

- Chips **Online · All · Pending · Blocked** (exist). Online opens first. No network icon on rows
  (ruling 4: only in mixed lists).
- **Online** tab: an **In voice** section first (friend, channel and server), then online, idle and
  dnd A to Z. **All**: online first, then A to Z (ruling 5). **Pending**: incoming first, then
  outgoing, newest first, each with Accept / Ignore or Cancel. **Blocked**: A to Z, Unblock.
- **Add friend** (P2): a field at the top of Pending, one username, user-typed, one confirm. No
  suggestions, no "people you may know", no import. Nothing sends without that click.
- **Mutual servers** are on the card only (`/users/{id}/profile` per person), never in a row.

## 5. Row anatomy (fields in priority order; drop from the right when narrow)

| Row | Always | On hover | Never | Missing field |
|---|---|---|---|---|
| DM (48, two lines) | avatar 32 + presence; name (friend nickname > global name > username); line 2; badge | Close DM (P2), time of last message | network icon, "Online" as text, handle | no line 2: one line, vertically centred |
| DM line 2, first that exists | "In a call" (accent glyph) > typing > activity ("Playing Hades", "Listening to Spotify") > custom status (emoji + text) > last message, dimmed | | elapsed time, rich-presence art | |
| Group DM | stacked faces (2) or its icon; name, or members' first names joined; line 2 "n members" or typing / call | member list | | unnamed: "Ines, Tomasz +2" |
| Server | icon 20 (squared); name; badge (accent mention count, else dot); friends-in-voice glyph + count | mute glyph if muted, "Mark read" | member count, boost, banner | no icon: initials tile |
| Folder | colour mark; name or first 3 server names; rollup | server count | | |
| Channel (28) | kind glyph (`#`, megaphone, speaker, stage, forum, lock for private); name; badge | mute glyph | topic (header only), role gates | |
| Thread (28) | `↳`; name; badge | | parent name | starter deleted: its own name |
| Occupant (28) | avatar 16; server nickname > global name; mute / deafen / live | | role colour | |
| Friend (48) | avatar + presence; name; line 2 = in voice > activity > custom status; Message button on hover | Call, more | "Online"/"Offline" text, network icon | no line 2: one line |
| Request (48) | avatar; name; @username (here it disambiguates strangers); Accept / Ignore or Cancel | | mutuals | |

Badge rule stays `Conversation::badge` [C]; a server's badge is its rollup. Accent only for mentions
and "in a call".

## 6. Discord specifics

- **Names by context**: server nickname inside that server (messages, occupants, member list);
  friend nickname > global name > username in DMs, People and Cmd-K. One `Person` per user id; the
  card shows nickname, global name and @username. Cmd-K matches all three.
- **Role colours: confirmed off** in rows and message names. Accent means "for you" (ruling 2), and
  role colours collide with it and with red. Roles appear as hoisted section headers in the member
  list and as a field on the card.
- **Bots / apps**: a small `APP` tag after the name everywhere. **System DMs** (Discord's own
  account): `SYSTEM` tag, no composer, never in requests, counted like any DM.
- **Mute levels**: server mute dims the server row and every channel; category mute dims and
  collapses it; channel mute dims the row. `suppress_everyone` / `suppress_roles` [C] mean those
  pings are plain unread. All of them still show a quiet mention count for a direct @me.
- **Message requests / spam**: DMs flagged as request or spam [U: `is_message_request`,
  `is_spam` on private channels] live only behind the Requests row, spam folded below. Opening one
  does not mark it read and does not reply. Accept is one explicit button.

## 7. Cost of each decision

| Decision | Cost |
|---|---|
| Folders, server order, mutes, read state, channel tree, threads joined, DMs, relationships | READY [C]; keep `guild_folders` (id, name, colour, ids) instead of flattening |
| In voice, friends-in-voice per server | READY_SUPPLEMENTAL `voice_states` + `VOICE_STATE_UPDATE` ∩ friends [C], in memory |
| Activity and custom status for friends | `merged_presences.friends` + `PRESENCE_UPDATE` [C]; keep activity `type` and `name`, drop the rest |
| Hide-muted, opt-in channels, NSFW, requests/spam flags, scheduled events | READY fields not yet parsed [U]; add to the named struct, no new request |
| Presence for a non-friend DM partner | not available; no dot (02 story 5) |
| Mutual servers, bio, pronouns | one REST call when the card opens; never per row |
| Server online/member counts, member list | guild subscription; only with the members pane open |
| Protocol additions | `Space.muted`, `Space.folder`, `Space.voice_friends`, `Person.activity`, `Conversation.request`, `Conversation.nsfw` |

## 8. Stories

| # | P | Story | Observable check |
|---|---|---|---|
| D1 | P0 | I open Discord and see Friends, then my DMs newest first, then my servers in my order. | Seed 20 DMs + 50 servers: first 10 DMs are in `updated` order; every DM with a number is shown even if older. |
| D2 | P0 | My server folders appear as I made them, collapsed, with their unread state. | Seed 3 folders: 3 Group rows in Discord order; a mention inside a collapsed folder shows that server row under it. |
| D3 | P0 | Clicking a server opens it; there is no inline expansion on home. | No chevron on server rows; click sets `Mode::Space`; back restores home scroll. |
| D4 | P0 | I see which friends are in voice, and where, without opening a server. | Friend joins a voice channel: "In voice" row appears on home and in the Online tab within 1 s; leaves: gone. |
| D5 | P0 | A DM row tells me if they are in a call, playing, or what their status says, never "Online". | Presence with activity type 0 "Hades": line 2 reads "Playing Hades"; status `online` with nothing else: one-line row. |
| D6 | P0 | A collapsed category or folder still shows what needs me. | Collapse all in Moss & Lichen: `#general`, `#showcase`, `#map-layers`, unread channels and Campfire remain. |
| D7 | P0 | Muted things look one way. | Muted DM, channel, server: dimmed, no glyph until hover, mention shows as quiet count. |
| D8 | P1 | A server hides the channels Discord hides from me (opt-in, hide-muted), and I can browse the rest. | Seeded opt-in server: tree count equals official client; "Browse n more channels" reveals the rest. |
| D9 | P1 | Big servers open tidy. | 300-channel seed: on first open only headers, numbered/unread channels and occupied voice rows. |
| D10 | P1 | Threads under a channel are only the ones I follow that have news. | Joined + read thread is not a row; "n threads" opens the list. |
| D11 | P1 | Message requests are in their own place and never badge me. | Seed a request DM: rail count unchanged; Requests row shows 1; opening it sends no ack. |
| D12 | P1 | Friends tab: Pending with Accept / Ignore / Cancel; Blocked with Unblock. | Accept: row moves to All without restart; official app agrees. |
| D13 | P2 | I add a friend by username. | One request per click; the field never autocompletes from server members. |
| D14 | P2 | A live or upcoming event shows at the top of its server. | Event starting in 2 h: one row; none: no row. |
| cut | | Server discovery, Nitro/boost UI, banners and profile effects, rich-presence art and timers, "Follow" announcement channel, server-wide online counts, mutuals in rows, role colours. | |

## 9. Open questions for the owner

1. **Activity text** ("Playing X", "Listening to Spotify") reverses a cut in `02`. It is free from
   presence and is what tells you a friend is free to talk. Recommended: show name only. Agree?
2. **Folders start collapsed** (recommended), or open the way you left them in Discord? Discord's
   own open/closed state may not be readable from READY.
3. **DMs before "Show more": 10** (recommended), or all open DMs with servers below the fold?
4. **Close DM** (removes it from the list, as Discord's ×): P2 on hover. Wanted in v1?
