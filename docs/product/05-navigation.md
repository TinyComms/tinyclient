# 05 · Navigation

How a person finds and moves between everything. Scope: rail, sidebar, browsing, threads and
forums, search, quick switcher, keyboard, history. Friends/people get a *place* here; what the
People view contains belongs to the contacts area.

Evidence tags: **[V]** checked against docs this pass, **[C]** already relied on by our adapter
code, **[U]** unverified (from memory of the networks' own clients; confirm on a real session).

## 1. One structure

Everything is one of five levels. The shell draws levels, never networks. Signal, Messages, and Tiny use the same levels. They are not in the table below.

| Level | Discord | Slack | Telegram | Teams |
| --- | --- | --- | --- | --- |
| **Network** | Discord | Slack | Telegram | Teams |
| **Space** | server; plus the built-in "Direct" space | workspace | none (one implicit space) | team; plus the built-in "Chats" space |
| **Group** (collapsible header) | category | sidebar section (Starred, custom, Channels, DMs) | chat folder; Archive | "Pinned", "Recent"; inside a team: none |
| **Conversation** | channel, DM, group DM, voice channel | channel, DM, group DM | private chat, group, channel, bot | chat, meeting chat, channel |
| **Sub-conversation** | thread, forum post | thread | forum topic, comment thread | channel post replies |

Rules that keep it one product:
- A row is always the 64 px conversation row. A Group is a 28 px header with a chevron. A Space is
  a row with a squared avatar. Nothing else exists in the sidebar.
- Inside a Space the order is **the network's own** (Discord `position`, Slack section order,
  Telegram folder order with pinned first, Teams' own order). People have years of muscle memory
  there. The Inbox is the only place sorted by recency.
- Three unread states everywhere: none, unread (heavier title, dot), mention (accent pill with a
  count). Muted rows are dimmed, never show a dot, and still show a mention pill.

## 2. Rail and sidebar

**Rail.** Inbox, People, one button per signed-in network, then the spaces the person pinned, add, and your avatar. Fifty Discord servers are not fifty rail icons. A network button's badge is a dot. The Inbox holds the number.

**Sidebar (316 px) has three modes, same components:**
1. **Inbox** (home, `cmd-1`). Recency list across networks with chips `All · Unread · Mentions`.
   Directs and group directs, starred channels, and channels that mention me. Not every unread channel on a busy server. That is the `Inbox` comment in `protocol`. Starred rows sit in a "Pinned" group on top.
2. **Network.** Groups for direct conversations first, then the network's Spaces as rows in the
   network's order (Discord server folders shown as Groups), each with its unread state. Telegram
   has no Spaces, so this mode shows its folders as Groups directly.
3. **Space.** One server/workspace/team: a header with its name and a back chevron, then
   Groups > Conversations in the network's order. Collapsed state is remembered per Group.

Multiple Slack workspaces are multiple accounts under one Slack sign-in, not extra Spaces of a single account. Tiny servers are added the same way, beside the first.

## 3. Stories

| # | P | Story | Observable check |
| --- | --- | --- | --- |
| N1 | P0 | I open the Inbox and see my direct conversations from all networks by recency, with chips for Unread and Mentions. | `Inbox` returns directs, group directs, starred rows, and mention rows. The Unread chip is the subset with unread. It is not every unread channel. |
| N2 | P0 | I click Discord and see every server I am in, in my own order, each showing whether it has unread or mentions. | `Spaces` for Discord lists the servers; order matches the official client. |
| N3 | P0 | I open a server and can open **any** channel I am allowed to read, under its category, in Discord's order. | `Tree` returns categories and channels. A channel the account cannot `VIEW_CHANNEL` is absent. Opening a never-mentioned channel returns messages. |
| N4 | P0 | I open a Slack workspace and see my channels and DMs under the same sections Slack shows. | `Tree` groups equal Slack's sidebar sections; every channel I am a member of is present, not just 30. The listing call is `client.counts`, not `client.userBoot`. |
| N5 | P0 | Collapsing a category/section sticks, and a collapsed Group still shows that something inside is unread. | Collapse, restart the shell: still collapsed; header carries the unread/mention state. |
| N6 | P0 | Muted channels are dimmed and never raise the server's or the rail's unread, unless I am mentioned. | Seed a muted channel with a new message: rail count unchanged; with a mention: +1. |
| N7 | P0 | `cmd-K` opens a switcher; I type a few letters of any channel, DM, person or server and Enter takes me there. | `Find` matches titles in the cache. A space never opened is absent until its tree has been fetched. Results appear within one frame of the keystroke. |
| N8 | P0 | I move through unread with the keyboard and mark things read without the mouse (section 7). | Scripted key events walk every unread row once and leave total unread at 0. |
| N9 | P0 | Scrolling up loads older messages without a jump; a pill takes me back to the bottom. | Scroll to top of 50: 50 more appear, the message under the cursor keeps its y position; pill shows new-message count and returns to newest. |
| N10 | P0 | When I open something with unread messages I land at a "New" divider, not at the bottom. | `open` returns `last_read_id` taken *before* marking read; divider drawn after it; it stays until I leave. |
| N11 | P0 | I click People and see my friends/contacts per network, and can start a DM from a row. | People lists Discord friends from READY `relationships`; Enter on a row opens or creates the DM. |
| N12 | P1 | I search and get results from this conversation, this network, or everywhere, and clicking one jumps to that message in context. | Query returns local hits immediately tagged "cached", server hits appended; clicking opens the conversation centred on the message, highlighted, with a "Jump to present" pill. |
| N13 | P1 | I open a thread next to its channel and keep reading the channel. | Thread opens in a right pane (pushes over the conversation below 1100 px width); `esc` closes it; channel scroll position unchanged. |
| N14 | P1 | A forum channel (Discord) or a topics group (Telegram) opens as a list of posts; a post opens like any conversation. | Opening a forum returns post rows (title, reply count, last activity), active first; opening one returns its messages. |
| N15 | P1 | My followed threads with new replies appear in the Inbox as rows titled "parent › thread". | A reply in a thread I posted in creates/updates one Inbox row; opening it clears it. |
| N16 | P1 | I star any conversation on any network and it is pinned at the top of the Inbox. | Star persists across restart; stored locally, nothing written to the network. |
| N17 | P1 | I browse Slack channels I have not joined, preview one read-only, and join it. | Not confirmed on this pass. Leave unchecked. Browse would list non-member public channels; preview would show history and a Join bar instead of the composer; after Join it is in the tree. |
| N18 | P1 | I pin a server or workspace to the rail and unpin it. | Pinned Space button shows that Space's own unread state; `cmd-digit` reaches it. |
| N19 | P1 | I go back and forward through where I have been, like a browser. | `cmd-[` and `cmd-]` replay the last 50 locations including scroll anchor. |
| N20 | P1 | I add a second Slack workspace and it appears as a second Space. | Two Spaces under Slack, separate link dots; signing out of one keeps the other. |
| N21 | P2 | I mark a whole server or folder read. | `shift-esc` on a Space clears every channel in it (Discord bulk ack, Slack per channel, paced). |
| N22 | P2 | Telegram chat folders and Archive appear as Groups I can collapse. | Groups equal Telegram chat folders (`Home` and `Folders`); a chat in two folders appears in both. grammers, not TDLib. |
| N23 | P2 | I browse and join public Telegram channels / hidden Teams channels. | Not confirmed on this pass. Leave unchecked. Public search would return joinable results; joining would add a row. |
| cut | — | Server discovery, Slack Connect invites UI, member-list browsing as navigation, saved searches, per-Space custom ordering that differs from the network's, split view of two conversations. | |

## 4. Per-network: what a user session gives, and what it costs

| Need | Discord | Slack | Telegram (schema; grammers in this client) | Teams |
| --- | --- | --- | --- | --- |
| Spaces + full tree at connect | READY carries every guild with channels, roles and my member entry **[C]**; nothing extra to request. Cost: one multi-MB payload at connect; parse streaming, write rows to SQLite, keep none in RAM. | `client.counts`, not `client.userBoot`. Sections from `users.channelSections.list` **[U]**. One listing call per workspace. | Chats load into the cache, then position updates. Folder updates **[U]**. Schema names, not TDLib. | One "teams/users/me" call on the client's CSA service returns teams, channels and chats **[U]**. |
| Hide what I cannot see | Discord hides channels the account cannot `VIEW_CHANNEL`. The bit is computed from `@everyone`, roles, and channel overwrites, and recomputed when those change. | Server only returns what I can see. | Same. | Same; "hidden" channels are a per-user flag **[U]**. |
| Unread / mention state | READY `read_state` vs channel `last_message_id`; unsubscribed guilds get `PASSIVE_UPDATE` unread deltas **[V]**. Mute from `user_guild_settings` **[V]**. | `client.counts` for every channel, DM and threads **[C]**. Mute list in user prefs **[U]**. | `unread_count`, `unread_mention_count`, `notification_settings` on each chat **[V]**. | Consumption horizon per conversation **[U]**. |
| Live messages for the channel on screen | Small servers: delivered unasked. Large servers: subscribe when the channel is open (op 14 / bulk op 37 **[U: numbers]**); drop the subscription on leaving. Whether mentions arrive from unsubscribed large guilds: **[U]**. | One websocket per workspace delivers everything I am a member of **[C]**. | The open chat is what the session treats as on screen. grammers, not TDLib. | One long-poll/trouter per account **[U]**. |
| Older history | `GET /channels/{id}/messages?before=` 50–100 **[C]** | `conversations.history` `latest=` + cursor **[C]** | `getChatHistory(from_message_id, offset, limit)` **[V]** | chatsvc messages `backwardLink` **[U]** |
| Jump to a message | `?around={id}` **[U]** | `conversations.history` `oldest`/`latest` + `inclusive`; `conversations.replies` for threads **[V]** | `getChatHistory` with negative offset **[V]** | fetch by time window **[U]** |
| Server search | `/guilds/{id}/messages/search`, `/channels/{id}/messages/search` for DMs; 25 per page; `202` + "index not yet available" means retry **[V]**. One guild per call; "everywhere" = fan-out we should not do. | `search.messages` with `in:` `from:` modifiers for user tokens **[V]**; web client uses `search.modules.*` **[U]**. Whole workspace per call. | Chat search and one-chat search, with filters. Schema names, not TDLib. Answers from this client's cache are `SearchMessages`, not a second database. | Substrate search service, separate token **[U]**; plan local-only first. |
| Threads / forums | Threads are channels with `parent_id`; joined ones arrive in `THREAD_LIST_SYNC` on subscription **[V]**; forum posts via thread search on the forum channel **[U]**. | `thread_ts` + `conversations.replies` **[V]**; followed-thread feed `subscriptions.thread.getView` **[U]**. | `getForumTopics`, `message_thread_id` **[U]**. | Each channel post is a root with replies **[U]**. |
| Friends / people | READY `relationships` **[V]** | no friends; people search endpoint **[U]** | `getContacts` **[U]** | people search **[U]** |

Costs that shape the design:
- **The tree lives in SQLite, not in memory.** A heavy account is ~50 servers x 100 channels + 500
  Slack channels: ~6,000 small rows. chatd answers from the table; the shell holds one Space.
- **Frames are capped at 1 MiB.** The shell asks for a page, not every channel of every server. The requests are below.
- **Opening is what subscribes.** No history, members or presence for a channel until it is opened.
- **Rate limits are the real latency** (Slack history ~50 calls/min; Discord search is tight). The
  tree is drawn from the boot payload; browsing never triggers a fetch per row.
- **Discord account risk:** one subscription change per navigation, debounced, at human pace.

## What the shell asks for

| Need | Request |
| --- | --- |
| Inbox, recency, Unread and Mentions chips | `Inbox` |
| Servers, workspaces, teams | `Spaces` |
| Categories and channels of one space | `Tree` (a page). `peek` for the switcher, so opening the index does not subscribe |
| Quick switcher | `Find` over titles in the cache. A space never opened is absent until its tree has been fetched. |
| Search the cache on this computer | `SearchMessages` |
| Search the network, on Enter | `SearchNetwork` |
| Telegram home, Discord DMs, a folder, the archive | `Home` and `Folders` |
| Followed threads, forum topics | `Threads`, `Topics` |
| Collapse, star | `Collapse`, `Star` (star is local) |
| Older messages, jump | `Messages` with a page. `last_read` is the divider, taken before mark-read. |

Discord hides channels the account cannot `VIEW_CHANNEL`. Slack's tree comes from `client.counts`, not `client.userBoot`. The tree is stored in SQLite. chatd does not keep every channel of every server in RAM.

## 5. Search

Two layers, one result list.
- **Local (every keystroke):** `SearchMessages` over the message cache. There is no FTS index. Offline,
  instant, covers only what has been opened or received. Rows are labelled "cached".
- **Network (on Enter):** the server search above, merged by message id. A line says what was
  searched: "Searched Slack · acme", or "Discord search isn't ready for this server yet" (the `202`).
- **Scope chip:** `This conversation` (`cmd-F`) · `This space` · `Everywhere` (`cmd-shift-F`).
  Everywhere = local on all networks + one server call per network with whole-account search
  (Slack per workspace, Telegram). A Discord server is searched only when it is the scope.
- **A result** is a message row under a "network icon · space › conversation · date" line, match
  emphasised. Enter opens the conversation around that message (N12). `from:`/`in:` filters are P2.

## 6. Quick switcher (`cmd-K`)

- **Matches** conversations whose space tree is in the cache, people, and Spaces, by
  subsequence on "space title". A space never opened is absent until its tree has been fetched. `#` limits to channels, `@` to people, a leading network name to it.
- **Empty query:** unread mentions, then the eight most recent places. `cmd-K, Enter` toggles
  between the last two conversations.
- **Ranking:** exact-prefix > word-prefix > subsequence; then mention > unread > frecency (visits
  decayed over two weeks) > DM > joined > not joined. Muted is pushed down, not hidden.
- Twenty 44 px rows, queried from chatd per keystroke; no index in the shell. `cmd-Enter` opens
  without marking read. A person with no DM yet opens a new one (created on first send).

## 7. Keyboard: the minimum set

| Keys | Action |
| --- | --- |
| `cmd-K` | Quick switcher |
| `alt-down` / `alt-up` | Next / previous row; with `shift`, next / previous **unread** (crosses Groups and Spaces) |
| `cmd-1` Inbox, `cmd-2` People, `cmd-3…9` | Networks, then pinned Spaces |
| `esc` | Close the top layer (switcher > search > thread); otherwise mark this conversation read and jump to newest |
| `shift-esc` | Mark the current Space read, after one confirm line |
| `cmd-shift-U` / `alt-click` a message | Mark unread from here |
| `cmd-[` / `cmd-]` | Back / forward |
| `cmd-F` / `cmd-shift-F` | Search here / everywhere |
| arrows, Enter, `left`/`right`, `cmd-down` | Move in the focused list, open, collapse/expand a Group, jump to newest |

Focus is always visible and is in exactly one of: sidebar, messages, composer, thread pane.

Rows that use `cmd` are macOS labels. The same commands exist in the menu (`OpenInNetwork`, `MarkRead` on shift-escape, the switcher). This pass did not diff every binding against `shortcuts.rs`.

## 8. History and position

- The shell holds 50 messages, growing to ~300; past that the far end is dropped and refilled from
  the cache. Older pages load within 10 rows of the top: cache first, network when it runs out.
- Scroll is anchored to the first visible message id, so inserts never move what I am reading.
- Away from the bottom, new messages do not scroll the view; a "Jump to newest · 3 new" pill appears.
  Opened from search, the window is centred on the hit and the pill reads "Jump to present".
- The "New" divider is fixed at open time (N10). Read state is `MarkRead` only when `may_mark_read` is true (`04-live-signals.md`), so opening does not eat unreads.

## Settled

The rail is Inbox, People, one button per signed-in network, pinned spaces, add, and your avatar. A network button's badge is a dot. The Inbox holds the number.

Multiple Slack workspaces are multiple accounts under one cookie. A second Tiny server is added beside the first.

The Inbox is directs and group directs, starred channels, and channels that mention me. Not every unread channel on a busy server.

Discord subscribes to a large server when one of its channels is open, and drops that subscription on leaving.

Search retention is 2,000 messages and 90 days (`KEEP_MESSAGES`, `KEEP_DAYS`). Settings › Privacy can shorten the age. The cache is unencrypted. See `07-budgets-and-risk.md`. There is no FTS index.

N17 (preview and join an unjoined Slack channel) and N23 (join a public Telegram channel) were not confirmed. Keyboard shortcuts were not diffed one by one against `shortcuts.rs`.
