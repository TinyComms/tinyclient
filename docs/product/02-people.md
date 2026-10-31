# 02 People

Friends and contacts, starting a conversation, avatars, presence, profiles, the member list.
Owned lists are held whole. A directory or a member list is a page for the rows on screen.
Presence is memory only.

## The rule that replaces the old cut

tinyclient knows about a person only when that person is on screen or in a list the user owns.

- **Owned lists are held whole**: Discord relationships (hard cap 1,000 friends), Telegram
  contacts, DM partners on every network. These are small.
- **Directories are searched, never downloaded**: a Slack workspace, a Teams org and a Discord
  server can be 100k people. tinyclient never pages through them. It asks the server for a search
  result or for the rows of the member list that are visible.
- **Presence is a subscription for what is visible**: friends and DM partners in the list, plus the
  people in the open conversation. Closing the conversation drops its subscription. Presence lives
  in memory only and is never written to SQLite.
- **A person row is cached on disk** (id, name, handle, avatar key, last seen status text) once
  seen, so reopening is instant and names resolve offline.

## Stories

Checks are the socket and the disk. The requests are:

| Story talk | Socket |
| --- | --- |
| people | `People` |
| profile | `Profile` |
| members | `Members` (a page, never the whole list) |
| start a conversation | `StartDirect` (nothing is sent until a message is) |
| set my status | `SetPresence` |
| accept, ignore, cancel, block | `Relate` |

| # | P | Story | Check |
| --- | --- | --- | --- |
| 1 | P0 | I see real avatars for people, group chats and servers everywhere a round or square initial shows today: conversation rows, message headers, the conversation header. | Rows from `list` and `open` carry an avatar reference; after first view a file exists in the avatar cache; with the network unplugged the same avatars still draw. |
| 2 | P0 | If a picture is missing, still loading or failed, I see the colored initials, never a blank or a spinner. | Block the image host: every row still renders initials and nothing reflows when the image arrives later. |
| 3 | P0 | I open People and see one list for every signed-in network, each row with avatar, name, network icon, presence dot and status text. What each network includes is the table below. | `People` returns rows with network, id, name, avatar, presence, status text; count for Discord equals the number of type-1 relationships. |
| 4 | P0 | People is ordered online first, then everyone else, A to Z inside each group. A network button in the rail narrows it, exactly as it narrows chats. | `People` with a network filter returns only that network; a friend going idle or offline moves groups within 5 s without a refetch. |
| 5 | P0 | I see a presence dot (online, idle, do not disturb, offline) on DM rows, People rows and the open conversation's header. Unknown is shown as no dot, never as offline. | Change status on a second account from the official app: the dot changes in tinyclient within 5 s. A Discord DM partner who is not a friend shows no dot. |
| 6 | P0 | I pick a person I have never talked to and start a conversation. It opens empty and my first message creates it on the network. | `StartDirect` returns a conversation id; the same call twice returns the same id; nothing appears in the other person's app until I send. |
| 7 | P0 | I type a name in People and find someone who is not in my list: a Slack or Teams colleague, a Telegram username, a member of a Discord server I am in. Search needs a query. The directory is not browsed A to Z. | `People` with a query returns server-side matches in under 1 s, at most 25 rows, and chatd memory does not grow by more than the rows returned. |
| 8 | P1 | I see custom status text and emoji next to a name, and "in a call" when the network reports it. | Status text set in the official app appears on that person's row and in the conversation header. |
| 9 | P1 | I click an avatar or name anywhere and get a profile card: large avatar, display name, handle, presence and status, plus the network's few extra fields. It has Message and, where the network has calls, Call. | `Profile` returns the fields in the Profiles table below in one round trip; the card paints from cache first. |
| 10 | P1 | I set my own status (online, away/idle, do not disturb, invisible where it exists) and status text once per network from the rail. | `SetPresence` succeeds and the official app on another device shows the change. |
| 11 | P1 | Discord: People has tabs Online, All, Pending, Blocked. I can accept or ignore an incoming request and cancel an outgoing one. | Pending shows types 3 and 4 with direction; `Relate` turns the row into a friend without a restart. |
| 12 | P1 | In a channel or group I open a member list on the right: sections with counts, online first, only the rows on screen loaded. | `Members` returns at most 100 rows plus section counts; scrolling a 100k-member server keeps chatd under its budget; closing the pane ends the subscription. |
| 13 | P2 | I add or remove a Discord friend, block a person, add a Telegram contact. | `Relate` changes the relationship in the official app. Adding by username is one at a time, user-typed only. |
| 14 | P2 | Animated avatars play on the profile card (as the autoplay setting says) and under the pointer in lists; otherwise a still frame. | A Discord `a_` avatar is still in rows until hovered and animates on the card. Built. |
| cut | | Server-wide or workspace-wide presence, "who is online in this server" counts beyond what the member list returns, last-seen history, activity/game detail ("Playing X for 2 h"), Spotify listen-along, avatar decorations and nameplates, profile banners and themes, mutual-friend graphs, importing the macOS address book to Telegram, org charts in Teams. | |

## One People view

One view, from the rail's People button. A network button narrows it. A person is not merged across networks.

| Network | In the list by default | What search adds |
| --- | --- | --- |
| Discord | Friends, pending, blocked | Members of servers I am in, by prefix |
| Slack | Direct and group-direct partners | The workspace directory, on the server |
| Telegram | Contacts (grammers, not TDLib) | Username search |
| Teams | People I already have a chat with | The org directory, on the server |
| Signal | Direct partners the adapter has | Not a server directory |
| Messages | The phone's conversations | The phone stays the address book |
| Tiny | Members of servers I am in, for the rows on screen | Tiny's own member page |

Presence is memory only. The `person` table stores id, name, handle, avatar, status text, and relationship. It has no presence column.

Discord member lists subscribe to the visible range of the open channel and drop it when the pane closes. Slack and Teams presence follows the directs and the open conversation, not a workspace crawl.

Accept, ignore, cancel, and block go through `Relate`. People search still needs a query. The directory is not browsed A to Z.

## Avatars

Where: conversation row, message header, conversation header, People row, member row (all one
"list" size, drawn up to 40 pt) and the profile card (96 pt). Group DMs use the network's group
icon if set, else the two most recent senders' avatars overlapped. Channels keep the squared `#`
tile; a Discord server channel shows the server icon. Fallback is today's initials on a gradient.

Rule: chatd fetches (it holds the sessions; Teams images need a token) and writes the original
bytes to `avatars/` under the app directory, named by network, id and the network's avatar hash, so
a changed picture is a new file and nothing is ever revalidated. Two source sizes only: list
(about 80 px) and card (about 256 px), card fetched on demand. The shell decodes an image when its
row becomes visible and drops it when the row leaves; decoded images held at once are capped
(about 200 list-size, roughly 5 MB). Disk cache is capped (about 50 MB) and evicted oldest-used
first. Signing out of a network deletes its avatars. At most 4 fetches in flight.

## Profile card: minimum useful

| Network | Fields beyond avatar, name, handle, presence, status text |
| --- | --- |
| Discord | Bio, pronouns, mutual servers, "friend since" or request state |
| Slack | Title, local time, email and phone if the workspace exposes them |
| Telegram | Bio, phone if shared, last seen wording |
| Teams | Job title, department, email, out-of-office note |

## What each protocol gives a user session

"Unverified" marks a cell that was not confirmed against the adapter in this pass. Telegram in this client is grammers. Names in that column that match the TDLib schema are the operation, not a library this client ships. Teams is a shipped adapter (`crates/chatd/src/adapters/teams.rs`). Discord speaks API v9. Signal, Messages, and Tiny are in the People list above. Their protocol cells were not filled in this pass.

| Capability | Discord (API v9) | Slack (xoxc + d cookie) | Telegram (grammers) | Teams (shipped adapter) |
| --- | --- | --- | --- | --- |
| The list | `READY.relationships` with type 1 friend, 2 blocked, 3 incoming, 4 outgoing, 5 implicit; also `GET /users/@me/relationships`. Live: `RELATIONSHIP_ADD/UPDATE/REMOVE`. | No friends concept. DM partners from `client.counts` / `conversations.list types=im,mpim`. `users.list` exists and is paginated, but must not be walked on a large workspace. | Contacts the account has, filled in as user records, plus contact search. Schema names, not TDLib. | No contacts list in the chat API; derive from chat rosters. Unverified against the adapter in this pass. |
| Search | `op 8` Request Guild Members with `query` prefix, max 100 results, one guild at a time. | Web client's edge API `edgeapi.slack.com/cache/{team}/users/search` (unverified); fallback `users.info` by id. | `searchPublicChat`, `searchChatsOnServer`, `searchContacts`. | Substrate people search `substrate.office.com/search/api/v1/suggestions` (unverified). |
| Start a conversation | `POST /users/@me/channels {recipients:[id]}`; idempotent, returns the existing DM. Opening a DM with a stranger is a signal Discord's anti-spam watches (unverified weight). | `conversations.open users=U1[,U2]`; idempotent; returns the im or mpim id. | `createPrivateChat(user_id)`; local, no network effect until a message is sent. | 1:1 thread id is derived, `19:{a}_{b}@unq.gbl.spaces`, created by `POST {chatsvc}/v1/threads` (unverified). |
| Avatar | `cdn.discordapp.com/avatars/{id}/{hash}.webp?size=N`, N a power of two 16 to 4096 plus extras including 80 and 96. `a_` hash prefix means animated. Default: `/embed/avatars/{(id >> 22) % 6}.png`. Server: `/icons/{guild}/{hash}`. Group DM: `/channel-icons/{channel}/{hash}` (unverified). Per-server avatar exists: `/guilds/{g}/users/{u}/avatars/{hash}`. No auth. | `profile.image_24/32/48/72/192/512` on the user object, plus `avatar_hash`. Workspace icon from `team.info`. Group DMs have no icon. Image hosts need no auth (unverified). | Profile photo small (160x160) and big (640x640), plus a minithumbnail for an instant blur, and the group photo. grammers does not keep a second file database. Schema names, not TDLib. | `{teams}/api/mt/{region}/beta/users/{mri}/profilepicturev2?size=HR64x64`, bearer token required (unverified). |
| Presence in | `READY.presences` / `merged_presences.friends`: non-offline friends and implicit relationships only. Live `PRESENCE_UPDATE`. Statuses online, idle, dnd, offline. Custom status is an activity of type 4 (`state`, `emoji`). No op to ask for a non-friend DM partner's presence. | Subscription only: send `{"type":"presence_sub","ids":[...]}` on the socket (the list replaces the previous one), receive batched `presence_change` (`active`/`away`). The adapter already passes `batch_presence_aware=1`. One-off: `presence_query` (500 ids) or `users.getPresence`. Status text arrives in `user_change`; do not disturb in `dnd_updated_user`. | `updateUserStatus` for known users. `userStatusOnline`, `Offline` (with time), or the privacy buckets `Recently`, `LastWeek`, `LastMonth`, `Empty`. No custom status text for non-Premium; Premium has an emoji status. | `POST presence.teams.microsoft.com/v1/presence/getpresence` with a list of MRIs; push through a pubsub subscription on the trouter socket. Availability plus activity (`InACall`, `Presenting`) and a note. All unverified. |
| In a call | Only via server voice states or `CALL_CREATE` on a DM; not shown for friends in general. | `profile.huddle_state` on the user object. | Not per person. | `activity` in presence (unverified). |
| Set my own | Gateway `op 3` (status, activities, since, afk) changes this session; the status that other devices keep is in the settings proto, `PATCH /users/@me/settings-proto/1` (unverified). Includes invisible. | `users.setPresence` (auto/away), `users.profile.set` for text, emoji and expiry, `dnd.setSnooze`. | `setOption("online", bool)`; `setEmojiStatus` is Premium only. | `PUT .../v1/me/forceavailability`, `.../v1/me/publishnote` (unverified). |
| Profile | `GET /users/{id}/profile?with_mutual_guilds=true`: bio, pronouns, badges, mutual guilds, connected accounts. | `users.info` (title, tz, email, phone, status); `users.profile.get` for custom fields. | `getUserFullInfo` (bio, common group count). | `fetchShortProfile` on the middle tier (unverified). |
| Member list | Gateway `op 14` (and bulk `op 37`) subscribes to ranges of 100 rows for one channel; the server answers `GUILD_MEMBER_LIST_UPDATE` with `SYNC/INSERT/UPDATE/DELETE` and role groups with counts. Large servers omit offline members. The fetched reference did not document these ops: unverified. | `conversations.members` cursor-paged ids, then names by id. No server-side ordering by presence. | `getSupergroupMembers` (200 per call, filters for recent and search); `getBasicGroupFullInfo`. Broadcast channels: admins only. | Roster in the thread properties, `GET {chatsvc}/v1/threads/{id}` (unverified). |

## Whose presence we never fetch

- Discord: no server is subscribed until one of its channels is open with the member list showing,
  and then only the visible ranges of that one channel. No `op 8` sweeps. No presence for message
  authors in server channels unless the member list already has them.
- Slack and Teams: the subscription set is DM partners visible in the sidebar or People, plus
  authors in the open conversation, capped at 500 ids, replaced as a whole when it changes.
- Telegram: only what the session already receives for contacts and open chats; tinyclient sends nothing extra. grammers, not TDLib.

## Settled, and one uncertainty

`Relate` is how a friend request, ignore, cancel, or block is sent. People search still needs a query. A directory is not browsed A to Z without typing. One People list, filtered by network. The same human on two networks is two rows.

Whether this client shows the person as online while the window is in the background was not re-checked against Slack and Teams presence heartbeats.
