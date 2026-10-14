# 06 Profile and info pane

What the right pane says about a person, a group, a channel and a Space on each network. When it opens, follows and closes
belongs to the navigation agent; this file owns what is in it. Read with `02-people.md` and `design/05-people-presence.md`.

## What exists (looked at on a seeded scratch daemon, 2026-10-13)

- A DM with the pane open shows head, presence and status, a bare "Friend" line, Message, Call, more, Media/Files/Links, and
  "Open in Discord". Message is useless beside the DM it would open. The empty tab says "No pictures or videos shared here
  yet." when it only means "none in the messages cached here": a partial list presented as complete.
- A channel or a group DM opens nothing: the header has only the members button.
- Data is mis-filed: Slack puts the job **title into `bio`** (drawn under "About"); Teams joins title, department and email
  into one `bio` block; Telegram returns bio only (no phone, username link, last seen, common groups); Discord drops pronouns,
  accent, banner, server nickname, roles and join date. `fields` is a free `(label, value)` list, so the shell cannot order,
  mask or name sections: every network's pane differs.

## Person: data matrix

Cost: **K** already known (row, READY, cache, presence push) · **1** in the one request made when the pane opens · **+1** a
second request, only on a click · **X** expensive, scope-gated or risky · — the network has no such thing.

| Field | Discord | Slack | Telegram | Teams | Verdict |
| --- | --- | --- | --- | --- | --- |
| Display name, picture | K | K | K | K | show (head) |
| Handle | K `username` | K `name` | K `@username`, may be none | — (sign-in address is email) | show if present (head) |
| Nickname in this server | 1 `guild_member.nick` (ask with `guild_id`) | — | 1 admin "custom title" in a group | — | show if present, under name: "Kit in Moss & Lichen" |
| Pronouns | 1 `user_profile.pronouns` | 1 `profile.pronouns` | — | — | show if present, after handle |
| Accent colour / banner | 1 `accent_color`, `banner` | — | K peer colour | — | accent band if present; banner image P2 |
| Presence | K friends only | K subscribed | K + last-seen bucket | K + activity | show (head); Unknown draws nothing |
| Custom status | K type-4 activity | K text, emoji, **expiry** | K emoji status (Premium) | K note | show with expiry ("until 5 PM") |
| Activity / in voice / huddle / meeting | K activities, voice states of shared servers | K `huddle_state` | — | K `InACall`, `Presenting` | one line if present: "In voice · #trail in Moss & Lichen" (P1, reverses the old cut: zero cost) |
| Last seen | — | — | K `wasOnline` or bucket | — | show in presence line: "last seen 2 h ago", "recently" |
| Out of office | — | (status convention only) | — | K presence OOF note (unverified) | banner if present |
| Bio / about | 1 | — (custom fields +1, P2) | 1 `about` | X (Graph "About me") | show if present, 4 lines then "more" |
| Title, department | — | 1 `title` | — (business intro: cut) | K cached `jobTitle`, `department` | show if present |
| Manager, org chart | — | — | — | X Graph scope | cut |
| Roles in this server | 1 role ids, names K | K admin / owner flag | 1 admin / owner in this group | K team owner | show if present, opened from a Space only: 6 chips + n |
| Email | — | 1 if workspace exposes | — | K | show if present, plain, click copies |
| Phone | — | 1 if exposed | 1 only if shared with me | X Graph | show **masked** "+44 •••• ••42 · Show" |
| Local time | — | 1 `tz_offset` | — | X mailbox settings | show if present: "4:12 PM · 3 h behind you" |
| Working hours | — | — | 1 business hours (rare) | X calendar | cut |
| On the network since | K id timestamp | — | — | — | foot line if present |
| In this server / group since | 1 `joined_at` | — | 1 participant date | — | foot line if present |
| Friends / contacts since | K relationship `since` | — | — | — | foot line if present |
| Mutual servers / groups | 1 `mutual_guilds` | — (each workspace is an account) | 1 `common_chats_count`; names +1 `getCommonChats` | — | show if present: names, 5 + n, each opens the Space |
| Mutual friends | +1 count flag (keep off by default) | — | K "mutual contact" | — | Discord P2; Telegram folds into relation |
| Marks: bot, app, verified, guest, external, scam / fake | K flags | K `is_bot`, `is_restricted`, `is_stranger` | K bot, verified, **scam, fake** | K bot, `#EXT#` guest, federated | one tag after the name; scam / fake in `danger` (P0) |
| Badges (Nitro, HypeSquad, boosts) | 1 | — | K Premium | — | never: vanity, not "who is this" |
| Connected accounts, stories, birthday, personal channel | 1 | — | 1 | — | cut (birthday P2 "show if present") |
| Private note | +1 `/users/@me/notes/{id}` | — | — | — | Discord P2, show if present, editable |
| Relation (friend, request, blocked, contact) | K | — | K contact | — | not a text line: it changes the actions; Blocked is a banner |
| Notifications for this person | K DM `muted` | K | K | K | toggle in the DM pane |

## The fixed order (one layout, every network)

Always present, in this order, on every network, in card and pane: **1 picture, 2 name + mark, 3 handle · pronouns (or
title when there is no handle: Teams), 4 presence + status.** These come from the row and paint before any request.

Then sections, each drawn only with content, never a label without a value, never "N/A", never a placeholder:

1. **Head** (above) + accent band when the network gives one, else no band (not grey).
2. **Actions** (below).
3. **Heads-up** one line, at most one, by severity: scam / fake, blocked, out of office, "not friends: they may not get it".
4. **Right now**: activity / in voice / huddle / meeting; local time.
5. **Role**: title · department (Slack, Teams); nickname and roles in this server (Discord, Telegram groups).
6. **About**: bio.
7. **Contact**: email, phone (masked), each click-to-copy. Never Discord.
8. **In common**: mutual servers / groups, then mutual friends count.
9. **Shared** (Media · Files · Links · Pinned) with this person.
10. **Foot**, one faint line: "On Discord since Dec 2017 · Friends since Mar 2021 · In Moss & Lichen since 2021", then
    "Open in Discord".

Section names are plain words, the same on every network: Role, About, Contact, In common, Shared. No "User ID", no "MRI",
no "Workspace member". Values carry the network's own words only where they are the user's (role names, nicknames).

## Actions on a person

| Action | Where | Discord | Slack | Telegram | Teams | Confirm |
| --- | --- | --- | --- | --- | --- | --- |
| Message | primary; hidden when the open conversation is already their DM | yes | yes | yes | yes | no |
| Call | primary where `can_call`; "Huddle in Slack ↗" hand-off label | native | hand-off | native | hand-off | no |
| Mute / unmute | primary (bell) in the DM pane; in more elsewhere | yes | yes | yes | yes | no |
| Accept / Ignore request | replaces Message while a request is pending | yes | — | — | — | no |
| Add friend / contact | more | P2, typed-intent only | — | add contact P2 | — | once |
| Remove friend | more | yes | — | delete contact P2 | — | once, names them |
| Block / unblock | more | yes | — | yes (P1) | P2 | once for block |
| Copy handle, copy link, copy id | more (id only with ⌥) | yes | yes | yes | yes (email as link) | no |
| Open in network app | foot | yes | yes | yes | yes | no |

Destructive (Remove, Block, Leave) are the last items in more, red, and ask once in a small confirm that names the person and
the network. Nothing in more is a network write without that click.

## Shared with this person or conversation

- The Shared section lists the **DM with this person**, whichever conversation is open (today it only lists when the DM is the
  open one, and disappears otherwise). In a channel or group pane it lists that conversation.
- Tabs: Media, Files, Links, Pinned. Pinned is P1 (Discord `/channels/{id}/pins`, Slack `pins.list`, Telegram pinned filter;
  Teams chats: unverified, hide the tab).
- **The honesty rule.** A list from the cache is labelled by what it is: "From messages loaded here · 14" and, below the last
  item, "Search all on Discord" (one click, never automatic). Empty from cache reads "None in the messages loaded here", never
  "No pictures shared yet". A count on a tab only when the network gave an exact total.

| Network | Complete, server-side | What tinychat shows |
| --- | --- | --- |
| Telegram | `messages.search` with photo-video / document / url filters; `getSearchCounters` gives exact totals | exact counts on tabs at open (one request); full lists on scroll: P1 |
| Discord | channel message search `has:image/file/link` (what the client does; unverified for DMs) | cache, "Search all on Discord" on click: P1 |
| Slack | `files.list channel=` for media and files; links by `search.messages has:link` | files complete on click: P1; links cache: P2 |
| Teams | chat files live in OneDrive / SharePoint; no confirmed route | cache only, always labelled; "Open in Teams" for the rest |

## Conversation info (recommendation: yes)

The header's info button opens **info for whatever is open**: a DM shows the person; a group, channel or Telegram channel shows
conversation info. One button, one pane, one order. The members button stays; info's "Members" row opens the same member pane.

| Section | Group DM | Channel | Telegram group / channel | Verdict |
| --- | --- | --- | --- | --- |
| Head | picture or two faces, name or member names, network chip | `#` / lock / speaker tile, name, Space › section | photo, name, @link, "group" / "channel" | always |
| Private / shared / external mark | — | K `private`; Slack Connect, Teams shared channel (missing) | public / private | tag if present |
| Actions | Call, Mute | Mute (All · Mentions · Nothing), Call where voice | Mute | primary |
| About | — | topic / purpose in full (not cut to 300) | `about` 1 | if present |
| Members | inline rows (≤ 10), else "Members 214" | "Members 128 · 12 online" row → member pane | count K, row → member pane | always where a count exists |
| Shared | Media · Files · Links · Pinned | same | same, exact counts | P1 |
| Foot | "Created Mar 2024" | "Created by Dev Patel · Mar 2024" (Slack 1; Discord date from id) | — | if present |
| Leave | more, confirm | cut (official app) | more, confirm (P2) | — |

**Thread**: no info pane of its own; the thread pane header already shows root and reply count; add participants' faces
(≤ 5) there, P2. **Space** (server / workspace / team): its own info, reached from the Space's name above the sidebar tree,
not from the conversation header: icon, name, description (Discord 1, Teams 1, Slack none), "1,204 members · 230 online"
(Discord counts K; Teams roster 1; Slack none), my nickname and roles there (Discord K), notification setting, "Open in". No
invite, no settings, no leave. P2.

## Privacy and restraint

- Profiles are asked **one at a time, when the pane or card opens**: never on hover, for a list, or ahead. One in-memory
  answer per person for 5 min (card → pane costs nothing); never written to SQLite or logs.
- Pacing per account: Discord one `Profile` per 3 s, superseded asks dropped (the route is watched and 429s), `guild_id` only
  when opened from that server, `with_mutual_friends` off; Telegram honours `FLOOD_WAIT`; Slack (tier 4) and Teams need none.
- Phone masked until clicked, each time; phone, email, bio and notes never reach the card, notifications, toasts or logs.

## Card versus pane: one design at two sizes

Keep both. The card (320 px, from any avatar or name) is the head, the actions, one heads-up line, **one** of Role or About
(two lines), and In common as one line of icons; then "View profile" docks the pane with the same answer, no second request.
The pane is the full order above. Same order, same words, same tags: the card is the pane's top third.

## Stories

| # | P | Story | Check |
| --- | --- | --- | --- |
| 1 | P0 | I open info in a DM and see picture, name, handle, presence in the same four places on all four networks. | Shots of a DM per network: the four items at identical offsets; no empty label anywhere. |
| 2 | P0 | Beside a DM, the pane does not offer "Message" to the conversation I am in. | Pane in a DM: no Message button; same pane from a channel member: Message present. |
| 3 | P0 | On Slack I see the title under Role, not under About; on Teams title, department and email are three lines in Role and Contact. | `Profile` answers carry title/department/email as typed fields; `bio` empty for Slack and Teams. |
| 4 | P0 | A Telegram "scam" or "fake" account carries a red tag next to the name, in the row, the card and the pane. | Fake account in the Telegram fake: tag drawn in all three. |
| 5 | P0 | An empty or partial Shared tab never claims to be complete. | Cache with 0 media: reads "None in the messages loaded here"; no tab count unless the answer says `total`. |
| 6 | P0 | Opening info on a channel or group shows its info, not nothing. | Header info on a channel opens a pane with name, topic, Members row, mute. |
| 7 | P1 | I mute a person or a channel from the pane and the official app shows it muted. | `SetMute` succeeds; row dims; the network's app agrees within 5 s. |
| 8 | P1 | Opened from a Discord server, I see their nickname and roles there and when they joined it. | `Profile {person, space}` answer has nick, roles, joined; opened from a DM it has none of those. |
| 9 | P1 | I see what someone is doing now: in voice and where, in a huddle, in a meeting, last seen. | Friend joins voice in a shared server: the Right now line appears within 5 s without reopening. |
| 10 | P1 | Telegram tabs show exact counts and the full list scrolls back to the first photo. | Counts equal the official app's shared-media counts. |
| 11 | P1 | "Search all on Discord / Slack" fills the tab from the server once, on click only. | No search request is sent until the click; result list says "All on Discord". |
| 12 | P1 | A phone number shows masked until I click Show. | Pane shot: masked; after click: full; reopening the pane masks it again. |
| 13 | P1 | The local time, out-of-office and status expiry read as a person would say them. | "4:12 PM · 3 h behind you"; "until 5 PM"; "Out of office until Mon". |
| 14 | P2 | Space info shows member and online counts and my roles there. | Discord server info matches the official counts ±1 %. |
| cut | | Badges, connected accounts, banners as images (until P2), org charts, managers, working hours, stories, Space invite / settings / leave, merged identities. | |

## What the contract lacks

- `Profile`: typed `title`, `department`, `email`, `phone`, `pronouns`, `nickname`, `roles: Vec<Role{name, colour}>`,
  `joined` (this Space), `friends_since`, `last_seen`, `away_until` / out-of-office text, `activity` (kind, name, where),
  `status_until`, `note`, `common_count`; `fields` kept only for leftovers. `Person`: `marks` (bot, verified, guest,
  external, scam, fake) instead of `bot: bool`.
- `Request::Profile` gains `space: Option<Key>` (context for nickname, roles, joined).
- `Request::SetMute { conversation, level }` with levels All / Mentions / Nothing; `Conversation.muted` becomes that level.
- `Request::ConversationInfo` → description in full, `members` and `online` counts, `created`, `creator`, `shared` / external
  mark, notification level, `pinned` count. `Request::SpaceInfo` → description, counts, my nickname and roles, notification.
- `SharedKind::Pinned`; `Response::Shared` gains `total: Option<u32>` and `source: Cache | Network`; `Request::Shared` gains
  `search: bool` for the server-side fill.
- `Relate` gains Unblock; adapters say which actions they support (today `offers()` guesses per network in the shell).

## Open questions for the owner

1. Phone masked until clicked (proposed), or plain as the official apps show it?
2. Discord "Playing / Listening / In voice" was cut in `02-people.md`; it costs nothing and the bar you showed has it. Bring it
   back as one line (proposed)?
3. Leave a group DM or Telegram group from the pane (behind a confirm), or keep every Leave in the official app?
