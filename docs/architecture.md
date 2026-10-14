# Architecture

How the processes fit together and who may edit what. The contract itself is the code in
`crates/protocol`; this file is the map.

## Processes, and what each may never do

| Process | Does | Never |
|---|---|---|
| `chatd` | Always on. Holds the sessions, the network sockets, SQLite and the media cache. Call signalling. | Decode media, link a codec or audio code, hold a tree, a member list or history in memory, fetch a link from a message, send anything the user did not ask for. |
| `mediad` | Exists from "connecting" to the end of one call. Voice gateway, UDP, Opus, DAVE, audio devices. | Run with no call, touch SQLite, the keychain or the chat socket. It speaks only to chatd over stdin and stdout. |
| `tinychat` | The window. Draws what chatd reports; decodes images for rows on screen. | Talk to a network, read the database or keychain, run a socket call, file read or decode on the main thread, run `mediad` itself. |
| `tinychat-login` | One sign-in window, then exits with the session on stdout. | Stay running. |

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
| `Ping`, `Status` | `Pong`; `Status {networks, accounts (link, detail, reconnecting, required, totals, own avatar, presence, status), media, call, seq}` | hub, store |
| `Wait {since}` | `Changes {seq, reset, changes}` | hub |
| `Inbox {network, filter, offset, limit}` | `Conversations {conversations, total, counts}` | store |
| `Find {network, query, limit}` | `Conversations` (title contains, newest first, cache only) | store |
| `Spaces {network}`, `Tree {space, offset, limit, peek}` | `Spaces`; `Tree {groups (kind, count), conversations, total}` | store; `Command::OpenSpace` |
| `Conversation`, `Collapse`, `Star`, `SetDraft` | `Conversation`; `Ok` | store (local only) |
| `People {network, relation, query, offset, limit}` | `People {people, total}` | store; `Command::SearchPeople`, debounced |
| `Members`, `StartDirect`, `Handoff`, `Profile {person}` | `Members`; `Conversation`; `Link`; `Profile` | adapter only (`Unsupported` until built) |
| `Relate {person, action}`, `SetPresence {account, presence, status}` | `Ok` | `Command::Relate` / `SetPresence` (`Unsupported` until built) |
| `Watch(Watching)` | `Watching {people, typists}` | hub; `Command::Watch` |
| `Messages {conversation, page, limit}` | `Messages {messages, last_read, older, newer}` | store; `Command::Backfill` / `History` / `HistoryAfter` / `HistoryAround` |
| `MarkRead {conversation, message_id}` | `Ok` | store; `Command::MarkRead` |
| `Send`, `Retry`, `Discard`, `Typing` | `Sent {message}` (pending); `Ok` | store; `Command::Send` / `Typing` |
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

## How the shell learns what changed

The hub keeps one entry per changed thing, each stamped with a rising `seq`; the same thing
changing again replaces its entry, so the list is bounded (256) by how many things there are.
`Wait {since}` blocks up to 30 s and answers every entry newer than `since`.

- Stored things name what to read again: `Status`, `Conversation`, `Messages`, `Spaces`, `Tree`,
  `People`, `Members`, `Media` (carries the file).
- Ephemeral things carry their whole value and cause no reads: `Typing {typists}` (each with
  `until`; the shell times the line out itself), `Presence`, `Call`, `Occupants`, `Searched`.
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
| Sessions | Keychain, service `tinychat`, one item per network. Never in SQLite. |
| Accounts, Spaces, groups (with their kind), conversations (with unread, mentions, muted, read marker, star, topic, thread parent and root, draft), people and relationships, messages (rich fields as JSON in `message.body`), media rows with their failure and size variants, adapter notes | `chat.sqlite`, schema version in `PRAGMA user_version` (now 3). A version change drops and refills the cache. |
| Media bytes | `media/<network>/<account>/` under the support directory, 500 MB, least recently used first. |
| Presence, typing, voice occupants, what the shell watches, links, the change list, the call | chatd memory, bounded, gone at restart. |
| Retention | Newest 2,000 messages per conversation; nothing older than 90 days unless the conversation was opened in that time. A thread row that names its parent goes when its parent goes, or when its root message has aged out and nobody opened the thread in 90 days. `Store::prune`, at start and daily. |

## What an adapter must do

- **Trees**: `Context::tree` writes a Space whole. A `Thread` row that names its `parent` is not
  the tree's to list: it survives a tree replacement and lives and dies with its parent. One
  group created, renamed or moved is `Context::group` / `group_gone`, with no tree resend.
- **A Space that keeps its direct conversations inside it** (a Slack workspace): the adapter
  lists its DMs and group DMs as rows of the Space's tree, with `space` set, under one `Group`
  whose `kind` is `GroupKind::Directs`, after the channel sections. They stay in the Inbox, which
  holds every direct conversation whatever its Space. Discord's DMs belong to no Space.
- **Calls**: set `call: Native` only on what tinychat can carry itself. A stage channel, and
  anything else a listener cannot simply join and speak in, is not `Native`.
- **Voice occupancy**: `Context::occupants(conversation, people, total)` for every voice channel
  of a Space on screen, whenever it changes, us included when we are in it.
- **Idle**: `serve_commands` blocks on the inbox with no timer, and `Context::pause` is the one
  interruptible wait. No loop of short sleeps: the whole process has a budget of about one idle
  wakeup a second.

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
(`demo/adapter.rs`) that answers for the three demo accounts: a send is accepted, a reaction
toggles, edit and delete work, a friend request can be answered, our presence set, a profile and
a member list read, a call in a voice channel puts us among its occupants. The server's own
tests use it as their network (`tests/e2e.rs`, `tests/calls.rs`). Variants, comma separated:
`live`, `expired`, `many` (50 Discord servers), `attention`, `reconnecting`. `TINYCHAT_DEMO_VOICE`
hands calls a voice server; `TINYCHAT_DEMO_ANSWER_MS` and `TINYCHAT_DEMO_SIGN_OUT_MS` set how long
the "network" takes to answer a ring and to sign out.

## Ownership

**Shared, edit only through the extension point:**

| File | Extension point |
|---|---|
| `crates/protocol/**` | Frozen. Ask the integrator; additions must be optional fields (`default`, `skip_serializing_if`). |
| `chatd/src/store.rs` | Frozen. Small lookups: `Context::note` / `set_note`. A new column is a `SCHEMA` bump by the integrator. |
| `chatd/src/adapters/mod.rs` | Frozen. Serve a `Command`, call a `Context` method. Unserved commands go to `Command::unsupported`. |
| `chatd/src/hub.rs`, `server.rs`, `session.rs`, `lib.rs`, `main.rs`, `demo.rs`, `demo/*`, `tests/e2e.rs` | Frozen. |
| `tinychat/src/gui/mod.rs`, `state.rs` | Slice D only. Other views keep their own `State` and take changes in their `*_changed` hook. |
| `Cargo.toml` files | Add only your own dependency lines. No async runtime, no second TLS stack. |

New files go in a directory named after your module and are declared from your own file
(`adapters/discord/*.rs` from `discord.rs`, `gui/people/*.rs` from `people.rs`), so no shared
`mod` list changes. Tests go in your own files or a new `tests/<slice>.rs`. A gap in the contract
is a `// CONTRACT:` comment where it was worked around, for the integrator.

| | Slice | May edit | Now implements |
|---|---|---|---|
| A | Calls | `crates/mediad/**`, `chatd/src/media.rs`, `adapters/discord/voice*`, `gui/call.rs`, `gui/call/*` | mediad: `Rejoin`, `Meter`, `DeviceList`, our own `Speaking`, `Ready {engine_linked: true}`. Adapter voice: `Context::occupants` for every voice channel on screen, `Context::answered`, `call: Native` never on a stage, `CallLeave` for a call it already left is nothing. Dock: `Call.speaking`, `level`, `inputs`, `outputs`; `Media(Meter)` around the device menu, no `mediad --devices-lines`; a failed call's words; occupants on the joined channel. |
| B | Adapters and media cache | `adapters/discord.rs`, `adapters/discord/*` (but `voice*`), `adapters/slack.rs`, `adapters/slack/*`, `chatd/src/blobs.rs`, `blobs/*`, `crates/login/**` | Commands: `React`, `Edit`, `Delete`, `Relate`, `SetPresence`, `Profile`, `HistoryAfter`, `HistoryAround`, `SignOut(reply)` answered after the network logout. Rows: `topic`, `read_only`, `private`, thread `parent` / `root`, `PersonRow.link`, Slack DMs under a `GroupKind::Directs` group. Messages: `forwarded`, `card`, `spoiler`, `sticker`, `waveform`, `ReplyTo.person`, `ThreadSummary.participants` / `unread`. Context: `attention`, `save_session`, `group`, `searched`, `members_changed`, `presence_of`, `stored_ids`, `media_variants`, `media_refreshed`; release a Space when `Watching.space` moves off it. Cache: `Store::media_failed` on a failed fetch and the reason on `Change::Media`, no fetch while `failed` is set, variants at 2x. |
| C | Shell conversation view | `gui/conversation.rs`, `gui/conversation/*` | `React`, `Edit`, `Delete`; `card`, `forwarded`, tombstones and deleted replies, `spoiler`, `sticker`, `waveform`; thread line with faces and unread; reply avatars from `ReplyTo.person`; `Fetch` with `size` (and `retry` only on the user's click), stop asking when `failed`; header subtitle from `Conversation::subtitle()`; `Page::After` / `Around`; never `MarkRead` under `gui-shot`. |
| D | Shell navigation, people and composer | every other file under `crates/tinychat/src/` | Space title on rows and in the switcher (`Find`); Inbox `counts` and the `Directs` filter; `Group.kind` / `count`; threads nested by `parent`; occupants on tree rows (`Change::Occupants`); `Watching.space`; `Relate`, `SetPresence`, `Profile`, `Person.link`; own avatar and presence from `Status`; `reconnecting` and `required` on a link; `SignIn.at` and the three sign-in errors; drafts through `SetDraft`; `Change::Searched` and `Change::Members`; `read_only` and `private` on rows. |

## Working in one checkout

- One cargo command at a time, `-j 6`. Slices A and B share the `chatd` crate, so someone else's
  half-written file can break your build: check with `cargo check -p chatd` and say so, do not fix it.
- Never run `chatd` without `TINYCHAT_DIR`. A scratch directory never reads the keychain; a test
  hands an adapter its session in `TINYCHAT_SESSION_DISCORD` / `TINYCHAT_SESSION_SLACK` and points
  it at a local fake with `TINYCHAT_DISCORD_API`, `TINYCHAT_DISCORD_GATEWAY`, `TINYCHAT_SLACK_API`,
  `TINYCHAT_SLACK_SOCKET`. In a scratch directory `SignIn` runs only a helper the test names in
  `TINYCHAT_LOGIN`, and a session an adapter saves goes to `session.<network>` in that directory.
- Left for later, with room kept: Telegram and Teams, message search, sending attachments,
  notifications, settings, video.
