# Polish 05 · The unified views: Inbox, People, rail, quick switcher

The places that mix networks; each network's own place belongs to its polish file. Looked at: demo
shots (Inbox, chips, People, switcher empty and `gen`, own menu, pinned Space) and the socket.

## 1. Why it feels "thrown at you"

| # | Cause | Where it shows |
| --- | --- | --- |
| F1 | **One weight for unequal things.** A Discord `#gear-swap` mention, a group DM with 12 unread and a DM from a friend are the same 2-line row with the same pill shape. | `inbox`: rows 1 to 13 |
| F2 | **Every row shouts its network.** A full-color brand glyph leads every title; with two networks that is 13 logos in one column, the loudest thing on screen. | `inbox`, `people` |
| F3 | **Where beats who.** The title line is `Space › title` and the Space gives up width first only in theory: `H… › Mara Okafor, Dev…`, `Fe… › Imogen Castellanos`, `Moss… › #showcase`. | `inbox` rows 1, 7, 9 |
| F4 | **No time structure, no cut-off.** 10 minutes ago and 8 days ago sit in one run; a DM with no message at all (`Wren Halloran`, `updated = 0`, empty preview) is in the Inbox. Real accounts add last year. | socket: last 4 rows |
| F5 | **Bots and apps are people.** The Inbox has no idea a row is an app (GitHub, Linear, Slackbot); `Conversation` has no bot flag. People shows Slack app DM partners as contacts with a tag. | integrator's accounts; `store.rs` `IN_INBOX` |
| F6 | **No preview on real accounts.** The preview is written only from stored messages (`store.rs` `preview_of`); Discord READY and Slack boot carry no message bodies, so every row is a name and a time until opened. The demo seeds messages, which hides it. | integrator's accounts |
| F7 | **Numbers counted twice.** Inbox 32 = Discord 22 + Slack 10. Three numbers on the rail say one thing. | `inbox` rail |
| F8 | **Channel rows vanish when read.** `IN_INBOX` holds a channel only while `mentions > 0`; open it and the row leaves the list. | `store.rs:189` |
| F9 | **Mixed People is an A-to-Z phone book.** 34 rows, Online then Everyone, with "Online" written under a green dot; a network toggle row repeats the rail. | `people` |
| F10 | **Switcher opens blank.** Empty query shows only the key hints, not "what needs me / where I was" (spec `05-navigation` §6). | `sw` |
| F11 | **Own avatar is a letter** on real accounts; the own picture is never fetched. | integrator |

## 2. The Inbox

**Its one job: the conversations I am in, newest first, and nothing I would not answer.** It is
not an index of everything (that is each network's home) and not a notification log.

Rules:
- **I1 Membership.** A row is in the Inbox when it is: a DM or group DM with a human; a channel I
  starred; a channel with a mention of me or a reply in a thread I am in, *for 24 h after that
  mention* (store `noticed_at`; fixes F8). Plain unread channels stay on their Space's dot. Confirmed.
- **I2 Cut-off.** Rows idle more than **30 days** are not in the Inbox unless starred, unread,
  holding a draft or a failed send. Rows with no message at all never are. The list ends with one
  quiet row: "Older conversations are in each network" with the network glyphs as links.
- **I3 Order is recency, not unread-first.** Unread-first makes a row jump when I read it. Order:
  Pinned (starred) group, then everything by newest message. Reading never moves a row.
- **I4 Day headers.** 28 px headers `Today`, `Yesterday`, `This week`, `Earlier` (the last 30 days).
  A header with no rows is not drawn. No headers inside the Pinned group.
- **I5 Apps fold.** A conversation whose other side is a bot or app (Slack app/Slackbot, Discord
  bot, Telegram bot, Teams bot/connector) is not a row. One **Apps** row sits at the end of its day
  group: stacked icons, "GitHub, Linear, Slackbot +2", a neutral dot if any is unread. Click
  expands it in place to 28 px rows. Apps never raise a number anywhere, except an app message that
  mentions me by name, which counts as a mention. chatd adds `bot: bool` to `Conversation` (peer is
  a bot, or the network marks the conversation as an app).
- **I6 Muted** rows stay in recency order, dimmed, no dot; a mention still shows its pill (existing rule).
- **I7 Chips.** `All · Unread n · Mentions n`, counts only on the last two, `n` hidden at 0.
  Unread = needs reading (not muted); Mentions = someone is waiting on me. Unread and Mentions are
  ordered mentions-first then newest: they are triage lists, and a read row stays until I leave
  the chip. No per-network chip: the rail's network button is that view. `Directs` stays API-only.
- **I8 Stability.** While the pointer is over the list or keyboard focus is in it, rows do not move;
  new activity collects in a "2 new ↑" pill at the top that applies them. The open row is tracked by
  key, not index: it keeps its highlight wherever it lands, and the keyboard cursor follows it.
- **I9 Preview must exist (F6).** chatd fills the preview of any Inbox row that lacks one: newest
  message, one request per row, only for rows on screen, at most one per second per network, never
  again once stored. Slack `conversations.history limit=1`; Discord `messages?limit=1` for DMs whose
  `last_message_id` is newer than what is stored; Telegram has `last_message`; Teams has it in the
  chat list. `updated` comes from the network's last-message time, never from sync time.

### Inbox row anatomy (48 px, 32 px avatar)

| Slot | DM | Group DM | Channel (starred / mentioned) | Apps row |
| --- | --- | --- | --- | --- |
| Avatar | person, presence corner | group icon, else 2 overlapped | **the Space's icon** (squared); `#` only if none | stacked app icons |
| Line 1 left | network glyph (14 px, monochrome `text_faint`, only when ≥2 networks signed in) + name | glyph + group name | glyph + `#name` then ` · Space` in `text_faint`; the Space truncates first, the channel never | "Apps" |
| Line 1 right | time: `10:12`, `Yesterday`, `Mon`, `Oct 2` | same | same | newest time |
| Line 2 | preview, see below | `Sender: preview` | `Sender: preview` | "GitHub, Linear +2" |
| Badge | neutral count of unread | neutral count | accent `@n` mention pill; dot if starred and plain unread | neutral dot |
| Slack, 2 workspaces | ` · Workspace` faint after the name | same | same | — |

Line 2, first match wins: failed send (red glyph + "Not sent") · typing (accent "Mara is typing")
· draft when no unread (pencil + "Draft: …") · missed call (red phone + "Missed call") · preview:
`You: ` for mine; `Sender: ` in groups and channels; attachment as glyph + word ("Photo", "2
photos", file name, "Voice message", "Sticker"); custom emoji as `:name:`; never empty (fallback
"No messages yet" is not drawn, because such rows are not listed, I2). Badges: accent fill means
"for me" (mention); a neutral count has no fill, just the number at `text_soft` semibold, so the
two never look alike (F1).

## 3. People across networks

**Mixed People answers "who could I talk to right now", not "who do I know".** The full lists are
each network's own (Discord Friends, Telegram Contacts, Slack/Teams DMs in their homes).

- **P1 Sections**, no chips: `Requests` as one row ("2 friend requests ›" opens Discord's Pending)
  only when non-empty · `In a call` (voice channel, huddle, Teams call; where known) · `Online`
  (online, idle, dnd) · `Recent` (talked to in 14 days, not online), each A to Z. Offline people
  I have not talked to recently are reached by search or the network's list. A last row: "All
  Discord friends · All Telegram contacts" as links.
- **P2 Bots are never in People** (`person.bot` filtered from owned lists and search results).
- **P3 Not merged.** The same human on two networks is two rows; A-to-Z puts them next to each
  other. No manual linking (cut: it decides where a message goes and invites mistakes).
- **P4 Search** matches name, display name, handle, folded for accents. Results: "Yours" (held,
  any network) first, then each network's server answer under its own header, then one line per
  network still asking. Drop the network toggle row (repeats the rail) and the "Online"
  subtitle (repeats the dot).

### People row anatomy (48 px, 32 px avatar)

| Slot | Content |
| --- | --- |
| Avatar | picture; presence shape bottom-right; unknown draws nothing |
| Line 1 | network glyph (monochrome, ≥2 networks) + name; bot never appears |
| Line 2, first match wins | status text (emoji + words) · "In Campfire" / "In a huddle" · Slack/Teams title or workspace when 2 workspaces · "Last message 3 d" · empty (row stays 48, name centred) |
| Right, on hover | Message · Call where the network has calls |

## 4. Rail

- **R1 One number.** Inbox carries the count of what needs me (DMs unread + mentions, unmuted).
  Network buttons and pinned Spaces carry only a dot: accent if a mention is inside, neutral if
  only unread. Dock badge = the Inbox number. Fixes F7.
- **R2 One button per network**, however many accounts (they are Spaces in its home). Pinned
  Spaces use the Space's own icon, reorder by drag, no cap (the rail scrolls); `cmd-digit` stops at 9.
- **R3 Own avatar** is the picture of the first account signed in; the own menu sets "Use this
  picture" per account. chatd fetches each account's own picture at sign-in (F11). The menu lists
  presence per account (as now) and "Set for all".
- **R4 Add** stays when all four are in: it adds an account (a second Slack workspace).

## 5. Quick switcher

- **Q1 Empty query**: `Needs you` (up to 5: mentions, then unread DMs) then `Recent` (8 last
  opened). Never blank (F10).
- **Q2 Rows** (44 px): avatar · title · faint `glyph Space › group` · badge. A person with a DM is
  shown once, as the DM. Ranking as `05-navigation` §6.
- **Q3 Actions**: Enter opens; `cmd-Enter` opens without reading; `alt-Enter` opens the profile in
  the right pane; a person without a DM starts one. Typing a network name first narrows to it.

## 6. Consistency versus fidelity: the rule

**Inside a network's own place its structure, order and words win; in a place that mixes networks
the shell's row and order win, and the only network-specific things shown are a monochrome glyph
and the Space name.** Examples: Telegram folders and pinned chats, Slack sections and Discord
server folders appear only in their homes, never in the Inbox; tinychat's star is the Inbox's only
pin. Mute and "what counts as unread" are always the network's (they are the user's settings).
Discord's "Friends" and Telegram's "Contacts" keep their names in their homes; mixed People says "People".

## 7. First minute and empty states

- One network signed in: no network glyph on any row. Syncing with nothing cached: one line per
  network, "Getting Slack · Harbor Labs", no spinner; rows arrive in place.
- Unread chip empty: check glyph, "You're all caught up", faint "Last: Ines, 10:09". Everything
  unread is muted: the same, plus "3 muted conversations have new messages" linking to them.
- People with nobody online: "Nobody's around", then the Recent section.

## 8. Stories

| # | P | Story | Observable check |
| --- | --- | --- | --- |
| U1 | P0 | I see a preview line on every Inbox row on my real accounts. | After connect, every row on screen has non-empty `preview` within 30 s; ≤1 fill request/s/network in the log. |
| U2 | P0 | I never see apps among people in the Inbox or People. | Slack app DM has `bot: true`; Inbox draws one Apps row; `People` has no `bot` rows. |
| U3 | P0 | Old dead conversations are not in my Inbox. | A DM idle 40 days with nothing unread is absent; starring it brings it back; `updated = 0` rows never listed. |
| U4 | P0 | I read a mentioned channel and it does not jump out from under me. | Open a mention row: still listed, same position, for 24 h. |
| U5 | P0 | A number appears once. | Rail shows a count only on Inbox; network buttons show dots; dock badge equals Inbox count. |
| U6 | P0 | I can tell rows apart at a glance. | Mention pill accent-filled, plain count unfilled; channel rows show the Space icon, title never truncated before the Space. |
| U7 | P1 | I see Today / Yesterday / This week groups. | Headers drawn only when non-empty; a new message moves a row to Today only when the pointer is off the list. |
| U8 | P1 | People shows who is around now, across networks. | Sections Requests/In a call/Online/Recent; an offline friend not talked to in 14 d is absent but found by search. |
| U9 | P1 | `cmd-K` with nothing typed shows what needs me and where I was. | ≤5 Needs-you rows then 8 Recent. |
| U10 | P1 | My own picture is on the rail. | Own avatar file exists after sign-in; the menu switches which account's picture. |
| U11 | P1 | "All caught up" is a real state. | Unread chip at 0 draws the caught-up block, not an empty list. |
| cut | — | Merging or linking the same person across networks; per-network chips in the Inbox; importing network pins into the Inbox; unread-first ordering in All. | |

## 9. Open questions for the owner

1. **Discord previews.** Discord's own client shows no DM previews; filling them is one
   `messages?limit=1` per visible DM, paced. Accept, or show Discord rows without a preview?
2. **Cut-off.** 30 days idle leaves the Inbox (starred, unread and drafts stay). Right number?
3. **People default.** "Around now + recent" with full lists in each network's home, or keep a
   mixed A-to-Z Everyone section?
4. **Own picture.** First signed-in account's picture by default: fine, or always ask at first run?
