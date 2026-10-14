# Architecture

How the processes fit together and who may edit what. The contract itself is the code in
`crates/protocol`; this file is the map.

## Processes, and what each may never do

| Process | Does | Never |
|---|---|---|
| `chatd` | Always on, one per support directory (`chatd.lock`). Answers its socket first, then reads the sessions behind it. Holds the sessions, the network sockets, SQLite and the media cache. Call signalling. | Decode media, link a codec or audio code, hold a tree, a member list or history in memory, fetch a link from a message, send anything the user did not ask for. |
| `mediad` | Exists from "connecting" to the end of one call. Voice gateway, UDP, Opus, DAVE, audio devices. | Run with no call, touch SQLite, the keychain or the chat socket. It speaks only to chatd over stdin and stdout. |
| `tinyclient` | The window. Draws what chatd reports; decodes images for rows on screen. Starts chatd when nothing answers (`protocol::launch`). | Talk to a network, read the database or keychain, run a socket call, file read or decode on the main thread, run `mediad` itself. |
| `tinyclient-login` | One sign-in window, then exits with the session on stdout. | Stay running. |

## Contract in brief (`crates/protocol`)

- **Frames**: 4-byte length, then JSON, at most 1 MiB. One request, one response, per connection or in sequence.
- **Identity**: an account is `AccountKey { network, account }`. A conversation, Space or person is a
  `Key { network, account, id }`. A Space is a server, workspace or team. One Slack sign-in is one
  keychain item that `adapters::accounts` splits into one account per workspace. An account id is
  one plain file name (letters, digits, `.`, `_`, `-`; never `.` or `..`): it names a directory.
- **Bounded**: rows are capped at `PAGE_LIMIT` 200 (a tree page at 500), and so is everything a
  network could make large: 3 thread faces, 25 occupants, 10 card fields, 256 waveform bytes, 32
  device names, 20 mutual Spaces, and a character cap on every string that is not a message's
  text. `Message::bound` applies the message's caps on the way into the store.
- **Additions are optional**: every field added after the first cut has `#[serde(default)]` and
  is left off the wire when empty, so an older frame still reads.

| Request | Answer | Served by |
|---|---|---|
| `Ping`, `Status` | `Pong`; `Status {networks, accounts (link, detail, reconnecting, required, totals, own avatar, presence, status, can), media, call, seq, keychain, started}` | hub, store |
| `ReadSessions` | `Status` (the read goes on behind it) | server: reads again the networks in `keychain.denied` |
| `Wait {since}` | `Changes {seq, reset, changes}` | hub |
| `Inbox {network, filter, offset, limit}` | `Conversations {conversations, total, counts (all, unread, mentions, apps)}` | store (rules below) |
| `Home {network, account, folder, shelf, offset, limit}` | `Conversations` (with this list's `counts`) | store; `Command::OpenHome` at offset 0 |
| `Folders {network}` | `Folders {folders}` (name, order, colour, Spaces, unread, mentions) | store |
| `Threads {space, offset, limit}`, `Topics {forum, offset, limit}` | `Conversations` | store; `Command::OpenThreads` at offset 0 |
| `InVoice {network}`, `Around {network}` | `Conversations`; `Around {requests, in_call, online, recent}` | hub (occupants, presence, activity) and store |
| `Find {network, query, limit}` | `Conversations` (title contains, newest first, cache only) | store |
| `Spaces {network}`, `Tree {space, offset, limit, peek}` | `Spaces`; `Tree {groups (kind, count), conversations, total}` | store; `Command::OpenSpace` |
| `Conversation`, `Collapse`, `Star`, `SetDraft` | `Conversation`; `Ok` | store (local only) |
| `People {network, relation, query, offset, limit}` | `People {people, total}` | store; `Command::SearchPeople`, debounced |
| `Members`, `StartDirect`, `Handoff` | `Members`; `Conversation`; `Link` | adapter only (`Unsupported` until built) |
| `Profile {person, space}` | `Profile` (typed, kept 5 min) | `Command::ProfileIn`, falling back to `Command::Profile` |
| `ConversationInfo {conversation}`, `SpaceInfo {space}` | `ConversationInfo {info}`; `SpaceInfo {info}` (kept 5 min) | `Command::ConversationInfo` / `SpaceInfo` |
| `SetMute {conversation, notify}`, `Leave`, `CloseDirect` | `Ok` once the network accepted | `Command::SetMute` / `Leave` / `CloseDirect` |
| `Pinned {conversation}` | `Pinned {conversation, messages}` (≤ 50) | `Command::Pinned` |
| `Shared {conversation, kind, before, limit, search}` | `Shared {items, more, complete, total}` | store; with `search`, `Command::SearchShared` |
| `Relate {person, action}`, `SetPresence {account, presence, status}` | `Ok` | `Command::Relate` / `SetPresence` (`Unsupported` until built) |
| `Watch(Watching)` | `Watching {people, typists}` | hub; `Command::Watch` |
| `Messages {conversation, page, limit}` | `Messages {messages, last_read, older, newer}` | store; `Command::Backfill` / `History` / `HistoryAfter` / `HistoryAround` |
| `MarkRead {conversation, message_id}` | `Ok` | store; `Command::MarkRead` |
| `Send {.., with}`, `Retry`, `Discard`, `CancelSend`, `Typing` | `Sent {message}` (pending, files listed); `Ok` | store; `Command::Send`, or `SendWith` when `with` is not plain; `CancelSend`; `Typing` |
| `Catalog {account, kind, space, query, cursor}` | `Catalog {items, cursor}` (≤ 200, kept 5 min) | `Command::Catalog` |
| `React {conversation, message_id, emoji, on}` | `Ok` | store at once; `Command::React` (`Unsupported` until built) |
| `Edit {.., text}`, `Delete {..}` | `Ok` once the network accepted | `Command::Edit` / `Delete` (`Unsupported` until built) |
| `Fetch {account, ids, size, retry}` | `Media {files}` | store (size, retry); `blobs` |
| `SignIn {network, at}`, `SignOut {account}` | `Status` | server |
| `CallStart`, `CallAccept`, `CallDecline`, `CallEnd`, `CallSet`, `Media(..)` | `Status` | `media::Calls`; `Command::Call*` |

- **Rows**: a `Conversation` carries its Space's and Group's titles and its topic
  (`Conversation::subtitle()` is the header's second line: `Space > group · topic`), `read_only`,
  `private`, the user's `draft`, and for a `Thread` its `parent` conversation and `root` message.
  A voice channel's row carries `occupants` when anyone is in it.
- **Messages** are one model: `text` plus `spans` (byte ranges on character boundaries: styles,
  links, mentions, custom emoji), `attachments` (file and thumbnail, dimensions known, `spoiler`,
  `sticker`, a voice message's `waveform` and `duration`), `previews`, `reactions`, `reply_to`
  (with the sender's `Key`, and `deleted` when the original is gone), `thread` (replies, the last
  three `participants`, `unread`), `forwarded` (where from; what was forwarded is the message's
  own text), `card` (a bot's embed: title, text, fields, footer, colour), flags, `kind` (normal,
  system, unsupported with a link) and `state` (sent, pending, failed with a reason). A deleted
  message that a thread hangs off stays as a tombstone (`deleted`, nothing else). No network
  markup reaches the shell.
- **Acting on a message**: a reaction shows at once (ours counts once, however often it is
  reported) and is taken back if the network refuses; a custom emoji that is not on the message
  yet waits for the network's word, which names it. Edit and delete change nothing until the
  adapter reports the result; the request's answer is the network's.
- **Media** is `MediaFile {id, kind, width, height, bytes, path, failed}`. `path` is `None` until
  the file is on disk. The shell asks `Fetch` for what is on screen; a file landing is
  `Change::Media`. A file that could not be fetched says why in `failed`, in plain words, and is
  not fetched again until `Fetch` says `retry` or the adapter registers a fresh address.
  `Fetch.size` is the longest side, in device pixels, of the whole picture at the scale it is
  drawn (for a cropped tile, the uncropped picture; points times the screen's scale). An adapter
  registers a file once at the size a row normally shows (`Context::media`) and then the other
  sizes the network serves (`Context::media_variants`, at most six, the longest side each). chatd
  points the file at the smallest registered size that is large enough, never at a smaller one
  than it holds, and a file pointed at a larger size reads as not fetched until it lands.
- **Unread** is three fields: `unread`, `mentions`, `muted`. `Conversation::badge()` is the one
  rule for number, dot or nothing; `AccountStatus.attention` and `Space.attention` are its sums.
  The Inbox filters ask different questions: `Unread` is anything unread or mentioning me that is
  not muted, `Mentions` is anything mentioning me, muted or not, `Directs` is direct and group
  conversations. Every `Inbox` answer carries the counts of all three tabs.
- **Read** happens only on `MarkRead`, which the shell sends when the window is active, the
  conversation is open and the message is on screen. `Messages` never marks anything. A shell
  built with `gui-shot` (screenshots, driven tests) never sends `MarkRead` at all: taking a
  picture of a conversation must not read it on the network.
- **Send**: the shell makes `client_id` (digits, at most 25, unique). `Send` stores a `Pending`
  message under that id and answers at once. The adapter ends it with `Context::sent`; the echo
  carrying the same `client_id` replaces the pending row. Pending for 60 s, or across a restart,
  becomes `Failed`. Nothing is queued while offline: `Send` is refused with `Offline`.
- **Never automated**: a friend request is answered, a presence set, a message edited, deleted or
  reacted to only by the shell's own request. No adapter does any of it on its own.
- **Links**: `SignedOut`, `Expired` (the network ended the session; cache and keychain item stay,
  sign-in replaces it), `Offline`, `Connected`, per account. Two details ride beside the link:
  `reconnecting` (offline, and the next attempt is under 30 s away, as opposed to backed off or
  never connected) and `required` (what the user must do in the network's own app, a captcha or
  a verification, in words; the adapter sets it with `Context::attention`).
- **Sign-in and sign-out**: `SignIn.at` is the main window's corner, passed to the helper as
  `--at X,Y`. The helper's exit says how it ended: 1 the window was closed (`SignIn`), 3 the page
  could not load (`SignInUnreachable`), 4 the organization blocks it (`SignInBlocked`). `SignOut`
  sends `Command::SignOut(reply)` and waits up to 5 s for the adapter to end the session on the
  network before it drops the adapter and forgets the account. A session part a network reissues
  while in use (Slack's rotated `d` cookie) is saved with `Context::save_session`.
- **People search** is debounced in the server: a query of at least 2 characters goes to the
  connected accounts of the request's network 400 ms after it last changed, the newest only, and
  the same query is not sent to an account twice within a minute. The adapter says
  `Context::searched` when it is done, found or not (`Change::Searched`).

### Added in the polish round (`protocol/src/info.rs` and the fields below)

- **Start and the keychain**: chatd binds and answers first, then reads each network's session on
  a thread (`Chat::read_sessions`). `Status.keychain` says `reading`, `waiting` (a read past one
  second: the shell shows "Waiting for permission to read your sign-in from the keychain") and
  `denied` (refused, cancelled or locked: cache kept, "Try again" is `ReadSessions`). Meanwhile
  a cached account is listed `Offline` with that in `detail`, never dropped. `Status.started`
  (unix ms) changes at every start: with `Changes.reset`, how a shell learns chatd restarted.
- **Starting chatd** is the shell's (`protocol::launch`): `chatd_answers`, `start_chatd` (its own
  process group, stderr to `chatd.log`), `chatd_path` (`TINYCHAT_CHATD`, else beside the running
  binary: the bundle's `Contents/MacOS` or `target/<profile>`; build both with
  `cargo build -p chatd -p tinyclient`). A second chatd finds `chatd.lock` held and exits.
- **Panics**: `protocol::crash::install_panic_hook` in chatd, mediad (and the shell) writes one
  redacted entry (`protocol::redact`) to stderr and `crash.log`. chatd answers a request that
  panicked with `Internal` and keeps the connection; every lock in chatd is taken with
  `hub::lock`, which recovers a poisoned one. A failed accept or thread spawn drops that one
  connection. A frame chatd cannot parse is answered `Unsupported` ("versions differ"); an
  answer over 1 MiB is `TooLarge`; `write_frame` refuses to write one.
- **Navigation**: a network may have no Spaces: its rows have no `space`, and its home is `Home`
  (the network's pins in `pin` order, then recency; `shelf` picks the archive or message
  requests). A row carries `folders`, `archived`, `pin` (the network's, apart from the local
  `starred`), `hidden`, `followed`, `bot`, `meeting` (the shell folds one quiet 14 days by
  `updated`), `special` (Saved, Service, Request), `mark`, `opted_out` (under "Browse n more"),
  and `quiet` (filled by chatd: nothing in 30 days, nothing unread, not starred, no draft). A
  folder (`Folder`) is a filter over chats (Telegram, membership on the row, the folder's own
  pins first) or a group of Spaces (Discord, `spaces`, `Space.folder`). `Change::Folders` says
  the list or a membership changed; the counts on folders move with unread, so a shell showing
  them reads `Folders` again on `Status` too. `Space` has `muted` and
  `hide_muted`; `Group` has `muted` (starts collapsed) and `unread_only`.
- **The Inbox** holds direct and group conversations, starred rows, rows the adapter lists, and
  channels that mention me, for 24 h after the mention, read or not. Never hidden rows, message
  requests, rows with no message, archived rows unless they mention me, or rows idle 30 days that
  are not starred, unread, drafted or failed. Bot rows are in it, `bot` set, for the one Apps
  row; their plain unread raises no number (`counts.apps` is their dot). Order: All starred then
  newest; Unread and Mentions mentions first. Day groups are the shell's, from `updated`.
  `Conversation::badge` and the SQL `ATTENTION` agree: bots and archived rows count mentions
  only; requests and hidden rows nothing.
- **Previews**: a row carries `preview`, `preview_from` (a group's last speaker), `preview_mine`
  and `seen` (the other side read my last message). The shell names the rows on screen in
  `Watching.rows`; chatd sends `Command::Previews` for those with none, each once, never to
  Discord.
- **People**: `Presence::Busy`; `Person.emoji` and `activity` (live, from `Context::activity`),
  `title`, `utc_offset`, `mark` (stored, `Context::details`). People and Around never list bots.
- **Profiles** are typed (`nickname`, `pronouns`, `title`, `department`, `roles`, `contacts` with
  phones `masked`, `mutual_total`, `joined`, `friends_since`, `last_seen`, `status_until`, `away`,
  `away_until`, `note`, `warning`); `fields` keeps leftovers only, and chatd moves a labelled
  line an adapter still sends (Pronouns, Title, Email, Phone, Roles) to its place.
- **Sending**: `Send.with` (`Outgoing`) carries `files` (paths, never bytes; ≤ 10 and the
  account's `max_files`), `silent`, `suppress_embeds`, `ping` (on by default), `pick` (a sticker
  or GIF by catalog id) and `schedule` (the network's own). chatd checks `AccountStatus.can`
  (`Capabilities`) first and refuses in words ("trail.mov is too big for Discord (limit 10 MB)").
  The pending message lists the files from their paths at once; progress is `Change::Upload`,
  at most four a second; `CancelSend` ends it `Failed` ("Cancelled"), and `Retry` sends the files
  again. Catalogs are asked when a picker tab opens, never before.
- **Shell memory** (last conversation, sidebar mode, Details switches, pane width, scroll
  anchors, remembered threads) is the shell's own prefs file, not chatd's: nothing else needs it.

## How the shell learns what changed

The hub keeps one entry per changed thing, each stamped with a rising `seq`; the same thing
changing again replaces its entry, so the list is bounded (256) by how many things there are.
`Wait {since}` blocks up to 30 s and answers every entry newer than `since`.

- Stored things name what to read again: `Status`, `Conversation`, `Messages`, `Spaces`, `Tree`,
  `People`, `Members`, `Folders`, `Media` (carries the file).
- Ephemeral things carry their whole value and cause no reads: `Typing {typists}` (each with
  `until`; the shell times the line out itself), `Presence` (with `emoji` and `activity`),
  `Call`, `Occupants`, `Searched`, `Upload {client_id, sent, total}`.
- `Occupants {conversation, occupants}` is who is in a voice channel: at most 25 people with
  mute, deafen and streaming flags, and the true total. It never announces the tree. The current
  value rides on the row whenever `Tree`, `Conversation` or `Inbox` is read; an emptied channel
  is told once with a total of 0. Held for 256 channels at most, never stored.
- `reset: true` means read everything on screen again: `seq` starts from the clock at each chatd
  start, so a shell that outlived a restart or fell more than 256 things behind is told so.
- Presence is kept only for owned lists and for `Watching.people` (500 at most, 2,000 entries in
  all); the rest is dropped on arrival. `Watch` lapses 90 s after the last `Wait`. Adapters get
  `Command::Watch` on change and should re-read `Context::watching()` on their heartbeat.
  `Watching.space` names the Space whose tree is on screen, open conversation or not: an adapter
  keeps a Space's live subscription only while it is named there.

## Where data lives

| Data | Where |
|---|---|
| Sessions | Keychain, service `tinyclient`, one item per network. Never in SQLite. |
| Accounts, Spaces (muted, hide muted), folders, groups (kind, muted, unread only), conversations (unread, mentions, muted, read marker, star, topic, thread parent and root, draft, filing, kind flags, preview parts, seen, when it last mentioned me), people and relationships (title, time zone, mark), messages (rich fields as JSON in `message.body`), media rows with their failure and size variants, adapter notes | `chat.sqlite`, schema version in `PRAGMA user_version` (now 4), page cache 1 MB. A version change drops and refills the cache: the 3 → 4 change does, once. |
| Media bytes | `media/<network>/<account>/` under the support directory, 500 MB, least recently used first. |
| Presence and activity, typing, voice occupants, what the shell watches, links and capabilities, the change list, the call, upload progress, previews asked, and five minutes of profiles, info panes and catalog pages (64 / 64 / 16 / 32 at most) | chatd memory, bounded, gone at restart. |
| Panics, chatd's stderr | `crash.log` (cut at 256 KB), `chatd.log` (cut at 4 MB at start) in the support directory. |
| Retention | Newest 2,000 messages per conversation; nothing older than 90 days unless the conversation was opened in that time. A thread row that names its parent goes when its parent goes, or when its root message has aged out and nobody opened the thread in 90 days. `Store::prune`, at start and daily. |

## What an adapter must do

- **Trees**: `Context::tree` writes a Space whole. A `Thread` row that names its `parent` is not
  the tree's to list: it survives a tree replacement and lives and dies with its parent. One
  group created, renamed or moved is `Context::group` / `group_gone`, with no tree resend.
- **A Space that keeps its direct conversations inside it** (a Slack workspace): the adapter
  lists its DMs and group DMs as rows of the Space's tree, with `space` set, under one `Group`
  whose `kind` is `GroupKind::Directs`, after the channel sections. They stay in the Inbox, which
  holds every direct conversation whatever its Space. Discord's DMs belong to no Space.
- **Calls**: set `call: Native` only on what tinyclient can carry itself. A stage channel, and
  anything else a listener cannot simply join and speak in, is not `Native`.
- **Voice occupancy**: `Context::occupants(conversation, people, total)` for every voice channel
  of a Space on screen, whenever it changes, us included when we are in it.
- **Idle**: `serve_commands` blocks on the inbox with no timer, and `Context::pause` is the one
  interruptible wait. No loop of short sleeps: the whole process has a budget of about one idle
  wakeup a second.
- **Filing**: `ConversationRow` carries `bot`, `meeting`, `special`, `mark`, `opted_out`, and
  `filing: Option<Filing>` (folders, archived, pin, hidden, followed): `None` leaves what is
  stored, so a tree reload never undoes a pin. One change alone is `Context::filing`; the
  folder list is `Context::folders` (`FolderRow`); a Space's mute and hide-muted
  `Context::space_flags`.
- **Previews and reads**: `Command::Previews(ids)` for rows on screen with none: one request a
  second at most, recorded with `Context::history` (a whole message) or `Context::preview` (a
  line). `Context::seen` when the other side read up to a message.
- **People**: `Context::details` (title, time zone, mark) after `Context::people`;
  `Context::activity` (emoji, playing, in voice, in a meeting) beside `Context::presence`.
- **Sending**: `Context::capabilities` once the session says what the account may send.
  `Command::SendWith` streams each file from its path in parts (one part in memory, 512 KB on
  Telegram, 8 MB elsewhere), calls `Context::upload` as it goes and `Context::sent` at the end
  (a failure names the file); `CancelSend` stops it.
- **New commands**, all answered `Unsupported` until built: `ProfileIn`, `ConversationInfo`,
  `SpaceInfo`, `SetMute` (report with `Context::muted`), `Leave`, `CloseDirect`, `Pinned`,
  `SearchShared`, `Catalog`, `SendWith`, `CancelSend`, `Previews`, `OpenHome`, `OpenThreads`.

## chatd and mediad (`protocol/src/call.rs`)

chatd writes `MediaRequest` frames to mediad's stdin; closing stdin is the hang-up. Every request
gets exactly one reply (`Streams`, `Done`, `Refused`), in order. Everything else on stdout is an
event at any time: `Connected {ssrc, dave_version}`, `Source {user_id, ssrc}`, `Speaking` (our
own user id reports us), `Gone`, `Levels`, `Devices`, `DeviceList`, `Interrupted`,
`Failed {reason, detail}` (then mediad exits).

A Discord call: `CallStart` is refused with `Unsupported`, and no mediad, unless the row's `call`
is `Native`. Otherwise it spawns mediad (`Connecting`) and sends the adapter `Command::CallJoin`.
The adapter updates its voice state on the gateway and, when it has both events, calls
`Context::voice(VoiceJoin {endpoint, token, session_id, user_id, server_id, channel_id, dave})`.
chatd forwards it as `MediaRequest::Join`; a second one for the same call (the voice server
moved) goes as `MediaRequest::Rejoin`. `Connected` makes the call `Joined`, the only state drawn
as "in a call". A call we rang stays `RingingOut`, whatever our own media does, until the adapter
says `Context::answered` (or lists someone else in the call). The roster comes from the adapter
(`Context::participants`); who is speaking comes from mediad and is merged by user id, ours into
`Call.speaking`. A ring is `Context::ring`, with no mediad until Accept.

Every way a call can fail ends the same way: the failure stays on screen in words until it is
dismissed, mediad is gone, and the adapter is sent `Command::CallLeave` so the network stops
showing us in the channel or ringing the other side. That covers a failure the adapter reported,
one mediad reported, a deadline here (10 s for the session, 35 s to connect, 60 s of ringing),
and mediad dying at any point, before the session arrived included.

The device menu: the shell sends `Media(Meter {on})` when it opens and closes. While on, mediad
sends `Levels` and chatd passes our input level on as `Call.level`; `Call.inputs` and
`Call.outputs` are every device the system lists (`DeviceList`), so the shell never runs
`mediad` to ask.

## The demo (`chatd --seed-demo`, `chatd/src/demo*`)

Invented data in a scratch `TINYCHAT_DIR`, and an adapter with no network behind it
(`demo/adapter.rs`) that answers for the five demo accounts (Discord, two Slack workspaces,
Telegram, Teams): a send is accepted, files "upload" from their paths with progress and can be
cancelled, a reaction toggles, edit and delete work, a friend request can be answered, our
presence set, a typed profile, a conversation's and a Space's info, a member list, pinned
messages, a shared-media search and the pickers' catalogs read, a mute set, a group left, a DM
closed, rows with no preview filled when on screen, and a call in a voice channel puts us among
its occupants. What each account holds is listed at the top of `demo.rs` (`demo/telegram.rs`,
`demo/teams.rs`, `demo/polish.rs` for this round). The server's own tests use it as their network
(`tests/e2e.rs`, `tests/calls.rs`, `tests/polish.rs`). Variants, comma separated: `live`,
`expired`, `many` (50 Discord servers), `attention`, `reconnecting`. `TINYCHAT_DEMO_VOICE` hands
calls a voice server; `TINYCHAT_DEMO_ANSWER_MS`, `TINYCHAT_DEMO_SIGN_OUT_MS` and
`TINYCHAT_DEMO_UPLOAD_MS` set how long the "network" takes to answer a ring, to sign out and to
take each quarter of a file. In a scratch directory only, `TINYCHAT_TEST_KEYCHAIN_MS` makes each
session read that slow and `TINYCHAT_TEST_PANIC=<query>` makes a `Find` for it panic while it
holds the store.

## Ownership (polish round)

**Shared, edit only through the extension point:**

| File | Extension point |
|---|---|
| `crates/protocol/**` | Frozen. Ask the integrator; additions must be optional fields (`default`, `skip_serializing_if`). |
| `chatd/src/store.rs` | Frozen. Small lookups: `Context::note` / `set_note`. A new column is a `SCHEMA` bump by the integrator. |
| `chatd/src/adapters/mod.rs`, `adapters/net.rs` | Frozen. Serve a `Command`, call a `Context` method. Unserved commands go to `Command::unsupported`, which every adapter's `match` already ends in. |
| `chatd/src/hub.rs`, `server.rs`, `session.rs`, `lib.rs`, `main.rs`, `media.rs`, `demo.rs`, `demo/*`, `tests/e2e.rs`, `tests/polish.rs`, `tests/calls.rs`, `crates/mediad/**` | Frozen. A demo answer a slice needs is a `// CONTRACT:` note for the integrator. |
| `tinyclient/src/gui/state.rs` | Slice A, except the `load_*` list functions (`load_inbox`, `load_spaces`, `load_tree`, `load_directs`, `load_row`, `refresh_row`), which are slice B's. Other views keep their own `State` and take changes in their `*_changed` hook. |
| `Cargo.toml` files | Add only your own dependency lines. No async runtime, no second TLS stack. |

New files go in a directory named after your module and are declared from your own file
(`adapters/discord/*.rs` from `discord.rs`, `gui/pane/*.rs` from `pane.rs`), so no shared `mod`
list changes; a new top-level `gui/<name>.rs` is declared in `gui/mod.rs` by slice A on request.
Tests go in your own files or a new `tests/<slice>.rs`. A gap in the contract is a
`// CONTRACT:` comment where it was worked around, for the integrator.

| | Slice | May edit | Implements or consumes |
|---|---|---|---|
| A | Shell foundation | `tinyclient/src/main.rs`, `client.rs`, `gui/mod.rs`, `gui/state.rs` (above), `gui/layout.rs`, `gui/theme.rs`, `gui/components.rs`, `gui/components/*`, `gui/assets.rs`, `gui/time.rs`, `gui/signin.rs`, `gui/signin/*`, new `gui/app/*` (menu bar, window states, overlays), `packaging/**` | Tokens and components of `design/polish/01-system.md`; every overlay (menu, popover, modal scrim) inside the window; window states from `Status.keychain` (`reading`, `waiting`, `denied` → `ReadSessions`), an account's `detail`, `reconnecting`, `required`; starting chatd with `protocol::launch::{chatd_answers, start_chatd}`, keeping the last snapshot behind "Reconnecting…", three restarts in 30 s, re-reading on `Status.started` changing or `reset`; socket timeouts in `client.rs`; `ErrorCode::TooLarge` and `Unsupported` ("versions differ") in words; `install_panic_hook("tinyclient")` and the crash-restart toast from `crash.log`; menu bar, ⌘W hides, window frame and prefs restored (last conversation, sidebar mode, Details switches, pane width). |
| B | Shell lists | `gui/sidebar.rs`, `gui/sidebar/*`, `gui/rail.rs`, `gui/people.rs`, `gui/people/list.rs`, `gui/people/hub.rs`, `gui/people/presence.rs`, `gui/people/status.rs`, the `load_*` functions of `gui/state.rs`, new `gui/home/*` | Inbox by `Inbox` (its rules are chatd's), day headers from `updated`, one Apps row from `bot` and `counts.apps`, chips from `counts`; `Watching.rows` for every list on screen (previews); People sections from `Around` (`in_call`, `online`, `recent`, `requests`), never bots; the rail's one number (`AccountStatus.attention`) and dots, its own picture fetched with `Fetch`; the switcher (`Find`, Needs you, Recent); the four homes: Discord (`Home` DMs, `Shelf::Requests`, `InVoice`, `Folders` of Spaces, `Space.folder` / `muted`, `opted_out` under "Browse", `Group.muted` collapsed, `followed` threads), Slack (`Tree`, `Group.unread_only`, `quiet` rows folded, `Threads`), Telegram (`Home` with `pin`, `Folders` chips and `folder`, `Shelf::Archive`, `Topics`, `special`, `mark`, `seen`, `preview_from`), Teams (`Home` chats, `meeting` 14-day fold, `special` Saved, `hidden`, teams' `opted_out` channels); `Presence::Busy`, `Person.emoji`, `activity`, `title`, `utc_offset` on rows. |
| C | Shell conversation and panes | `gui/conversation.rs`, `gui/conversation/*` (but `emoji.rs`), `gui/people/members.rs`, new `gui/pane.rs`, `gui/pane/*` | **P0**: a thread composer never posts to the conversation the user left (put the thread away on every switch, R2). Right pane kinds and following (07 §1), one width 300 (260–420), landing at the first unread, per-conversation memory (scroll anchor, remembered thread, last 50), Escape ladder; header from `Conversation::subtitle()`, `read_only`, `special` (no composer for Service), `mark`; `Pinned` for the pins; `Change::Upload` progress and Cancel (`CancelSend`) on a pending row; a pending row keeps its place and time; `rich.rs` span ranges never panic. |
| D | Shell profile, info pane and card | `gui/people/card.rs`, `gui/people/info.rs`, `gui/people/shot.rs`, new `gui/profile/*` | `Profile {person, space}` with the typed fields in the fixed order (06), `contacts` masked until clicked, `warning`, `away`; `ConversationInfo` (description, members, online, created, creator, `notify`, `pinned`, `external`, `link`) and `SpaceInfo` from the Space's name; `SetMute`, `Leave` (one confirm), `CloseDirect`, `Relate`; `Shared` from the cache labelled as such (`complete: false`) and "Search all" as `search: true`, counts only from `total`; the floating card 300 wide, 64 avatar. |
| E | Shell composer and pickers | `gui/composer.rs`, `gui/composer/*`, `gui/input.rs`, `gui/conversation/emoji.rs`, new `gui/picker/*` | `Send.with` (`Outgoing`): attach, paste and drop staging paths (pasted pixels to `$TINYCHAT_DIR/outbox/`), per-file `spoiler`, `as_file`, `original`, refusals checked against `AccountStatus.can` before staging; `silent` (Option-Return where `can.silent`), `suppress_embeds` (the preview chip), `ping` on replies, `pick` from the picker; one picker with Emoji (Unicode list local, custom from `Catalog` `Emoji`), GIFs and Stickers tabs, each only where `can` allows, asked on tab open; autocomplete; the `@everyone` confirm from `ConversationInfo.members`; `input.rs` IME ranges never panic. |
| F | Discord and Slack adapters | `adapters/discord.rs`, `adapters/discord/*`, `adapters/slack.rs`, `adapters/slack/*`, `crates/login/**`, `tests/discord.rs`, `tests/slack.rs`, `tests/fixtures/{discord,slack}/**` | Discord: `guild_folders` → `Context::folders`, server mute and hide-muted → `space_flags`, opt-in → `opted_out`, muted categories → `Group.muted`, message requests → `Special::Request`, bot DMs `bot`, joined threads `followed`, `Context::activity` from presences and voice states (friends in voice reported as occupants), `ProfileIn` (nickname, roles, joined), `ConversationInfo`, `SpaceInfo`, `SetMute`, `Leave`, `CloseDirect`, `Pinned`, `SearchShared`, `Catalog` (READY's emoji and stickers, `/gifs/search`), `SendWith` (attachments, `SPOILER_`, flags `silent` / `suppress_embeds`, `allowed_mentions.replied_user`), `capabilities` from `premium_type`; never a per-row preview fetch. Slack: sections' `unread_only`, app and bot DMs `bot` (Slackbot too), guests and external `mark`, `details` (title, `tz_offset`), status emoji through `activity`, huddles, `Previews` (`conversations.history limit=1`, paced), `OpenThreads` (`subscriptions.thread.getView`) → `followed`, `ConversationInfo`, `SetMute`, `Pinned`, `SearchShared` (`files.list`), `Catalog` (`emoji.list`), `SendWith` (`files.getUploadURLExternal` …), `schedule`. |
| G | Telegram and Teams adapters | `adapters/telegram.rs`, `adapters/telegram/*`, `adapters/teams.rs`, `adapters/teams/*`, `tests/telegram.rs`, `tests/teams.rs`, `tests/fixtures/{telegram,teams}/**` | Telegram: no Space (rows with no `space`), `filing` (folders from the dialog filters' rules, archive, pins), `Context::folders` with the folders' own pins, `listed` only for people, groups, bots and the service chat, `special` Saved and Service, `mark` (verified, scam, fake), `seen` from `read_outbox_max_id`, topics as `Thread` rows under the forum with their pins, `OpenHome`, `Previews` (paced), `ProfileIn` (admin title), `ConversationInfo`, `SetMute`, `Leave`, `Pinned`, `SearchShared` (with counters), `Catalog` (saved GIFs, `@gif`, sticker packs), `SendWith` (`saveFilePart` in 512 KB parts, albums, spoiler, as file), `silent`, `schedule`. Teams: chats with no Space, `meeting`, bot chats `bot`, Notes `Special::Saved`, `hidden` (unhidden by a new message), team channels' `opted_out`, `Presence::Busy` and `activity` (in a meeting, presenting), out of office in `Profile.away`, `details` (title, department), `Context::preview` from `lastMessage`, `seen`, `ConversationInfo`, `SetMute`, `SendWith` (OneDrive, second token). |
| H | Media and sending in chatd | `chatd/src/blobs.rs`, `blobs/*`, new `chatd/src/upload.rs`, `upload/*`, `tests/blobs.rs`, new `tests/upload.rs` | One upload helper for F and G: a file read from its path in bounded parts, progress through `Context::upload`, cancel, a failure that names the file; the catalog previews' files under `media/<network>/pick/`, deleted when the picker closes, custom emoji in the 48 px cache; the on-screen rows' avatars and previews fetched with at most 4 in flight; each account's own picture fetched at sign-in (the rail's). |
## Working in one checkout

- One cargo command at a time, `-j 6`. Slices A and B share the `chatd` crate, so someone else's
  half-written file can break your build: check with `cargo check -p chatd` and say so, do not fix it.
- Never run `chatd` without `TINYCHAT_DIR`. A scratch directory never reads the keychain; a test
  hands an adapter its session in `TINYCHAT_SESSION_DISCORD` / `TINYCHAT_SESSION_SLACK` and points
  it at a local fake with `TINYCHAT_DISCORD_API`, `TINYCHAT_DISCORD_GATEWAY`, `TINYCHAT_SLACK_API`,
  `TINYCHAT_SLACK_SOCKET`. In a scratch directory `SignIn` runs only a helper the test names in
  `TINYCHAT_LOGIN`, and a session an adapter saves goes to `session.<network>` in that directory.
- Slices F, G and H share the `chatd` crate, and A to E the `tinyclient` crate: someone else's
  half-written file can break your build. Check with `cargo check -p chatd` (or `-p tinyclient
  --features gui`) and say so; do not fix it.
- Left for later, with room kept: message search, notifications, settings, video, voice
  messages (P2), Discord's per-server file limit (boosts) and Telegram's boosted packs in
  `Capabilities`, picker recents in SQLite.
