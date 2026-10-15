# 02 People

Friends and contacts, starting a conversation, avatars, presence, profiles, the member list.
This replaces the cut "Member lists and presence for whole servers or workspaces" in
`docs/stories.md` with a bounded version. The README line "does not scrape member lists, or track
presence" must be reworded by the owner when this ships (see question 1).

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

Checks are observable on the chat socket or on disk. `people`, `person`, `members`, `start` and
`set_status` are proposed request names, not existing ones.

| # | P | Story | Check |
| --- | --- | --- | --- |
| 1 | P0 | I see real avatars for people, group chats and servers everywhere a round or square initial shows today: conversation rows, message headers, the conversation header. | Rows from `list` and `open` carry an avatar reference; after first view a file exists in the avatar cache; with the network unplugged the same avatars still draw. |
| 2 | P0 | If a picture is missing, still loading or failed, I see the colored initials, never a blank or a spinner. | Block the image host: every row still renders initials and nothing reflows when the image arrives later. |
| 3 | P0 | I open People and see my Discord friends, Slack and Teams DM partners and Telegram contacts in one list, each row with avatar, name, network icon, presence dot and status text. | `people` returns rows with network, id, name, avatar, presence, status text; count for Discord equals the number of type-1 relationships. |
| 4 | P0 | People is ordered online first, then everyone else, A to Z inside each group. A network button in the rail narrows it, exactly as it narrows chats. | `people` with a network filter returns only that network; a friend going idle or offline moves groups within 5 s without a refetch. |
| 5 | P0 | I see a presence dot (online, idle, do not disturb, offline) on DM rows, People rows and the open conversation's header. Unknown is shown as no dot, never as offline. | Change status on a second account from the official app: the dot changes in tinyclient within 5 s. A Discord DM partner who is not a friend shows no dot. |
| 6 | P0 | I pick a person I have never talked to and start a conversation. It opens empty and my first message creates it on the network. | `start {network, person}` returns a conversation id; the same call twice returns the same id; nothing appears in the other person's app until I send. |
| 7 | P0 | I type a name in People and find someone who is not in my list: a Slack or Teams colleague, a Telegram username, a member of a Discord server I am in. | `people {query}` returns server-side matches in under 1 s, at most 25 rows, and chatd memory does not grow by more than the rows returned. |
| 8 | P1 | I see custom status text and emoji next to a name, and "in a call" when the network reports it. | Status text set in the official app appears on that person's row and in the conversation header. |
| 9 | P1 | I click an avatar or name anywhere and get a profile card: large avatar, display name, handle, presence and status, plus the network's few extra fields. It has Message and, where the network has calls, Call. | `person {network, id}` returns the fields in the Profiles table below in one round trip; the card paints from cache first. |
| 10 | P1 | I set my own status (online, away/idle, do not disturb, invisible where it exists) and status text once per network from the rail. | `set_status` succeeds and the official app on another device shows the change. |
| 11 | P1 | Discord: People has tabs Online, All, Pending, Blocked. I can accept or ignore an incoming request and cancel an outgoing one. | Pending shows types 3 and 4 with direction; accept turns the row into a friend without a restart. |
| 12 | P1 | In a channel or group I open a member list on the right: sections with counts, online first, only the rows on screen loaded. | `members {conversation, from, count}` returns at most 100 rows plus section counts; scrolling a 100k-member server keeps chatd under its budget; closing the pane ends the subscription. |
| 13 | P2 | I add or remove a Discord friend, block a person, add a Telegram contact. | The relationship changes in the official app. Adding by username is one at a time, user-typed only. |
| 14 | P2 | Animated avatars play on the profile card (as the autoplay setting says) and under the pointer in lists; otherwise a still frame. | A Discord `a_` avatar is still in rows until hovered and animates on the card. Built. |
| cut | | Server-wide or workspace-wide presence, "who is online in this server" counts beyond what the member list returns, last-seen history, activity/game detail ("Playing X for 2 h"), Spotify listen-along, avatar decorations and nameplates, profile banners and themes, mutual-friend graphs, importing the macOS address book to Telegram, org charts in Teams. | |

## One People view, not four

One view, reached from a Chats | People switch at the top of the sidebar. Rows are the same row on
every network; the rail's network buttons filter it. Sections: **Requests** (Discord incoming, only
when non-empty), **Online**, **Everyone**. A person is not merged across networks: the same human
on Slack and Discord is two rows, because merging guesses identity and sends to the wrong place.

| Network | What "People" contains by default | What search adds |
| --- | --- | --- |
| Discord | Relationships: friends, pending in and out, blocked (hidden behind its tab) | Members of servers I am in, by name prefix |
| Slack | People I have a DM or group DM with, most recent first inside each group | The workspace directory, server-side |
| Telegram | Contacts | Global username search, people in my chats |
| Teams | People I have a 1:1 or group chat with | The org directory, server-side |

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

"Unverified" means from memory of the unofficial client protocol, not confirmed against a source
during this research. Confirm against a live session before building.

| Capability | Discord (gateway v10 + REST) | Slack (xoxc + d cookie) | Telegram (TDLib) | Teams (client chat API) |
| --- | --- | --- | --- | --- |
| The list | `READY.relationships` with type 1 friend, 2 blocked, 3 incoming, 4 outgoing, 5 implicit; also `GET /users/@me/relationships`. Live: `RELATIONSHIP_ADD/UPDATE/REMOVE`. | No friends concept. DM partners from `client.counts` / `conversations.list types=im,mpim`. `users.list` exists and is paginated, but must not be walked on a large workspace. | `getContacts` returns user ids; `updateUser` fills them; `searchContacts`. | No contacts list in the chat API; derive from chat rosters. All unverified. |
| Search | `op 8` Request Guild Members with `query` prefix, max 100 results, one guild at a time. | Web client's edge API `edgeapi.slack.com/cache/{team}/users/search` (unverified); fallback `users.info` by id. | `searchPublicChat`, `searchChatsOnServer`, `searchContacts`. | Substrate people search `substrate.office.com/search/api/v1/suggestions` (unverified). |
| Start a conversation | `POST /users/@me/channels {recipients:[id]}`; idempotent, returns the existing DM. Opening a DM with a stranger is a signal Discord's anti-spam watches (unverified weight). | `conversations.open users=U1[,U2]`; idempotent; returns the im or mpim id. | `createPrivateChat(user_id)`; local, no network effect until a message is sent. | 1:1 thread id is derived, `19:{a}_{b}@unq.gbl.spaces`, created by `POST {chatsvc}/v1/threads` (unverified). |
| Avatar | `cdn.discordapp.com/avatars/{id}/{hash}.webp?size=N`, N a power of two 16 to 4096 plus extras including 80 and 96. `a_` hash prefix means animated. Default: `/embed/avatars/{(id >> 22) % 6}.png`. Server: `/icons/{guild}/{hash}`. Group DM: `/channel-icons/{channel}/{hash}` (unverified). Per-server avatar exists: `/guilds/{g}/users/{u}/avatars/{hash}`. No auth. | `profile.image_24/32/48/72/192/512` on the user object, plus `avatar_hash`. Workspace icon from `team.info`. Group DMs have no icon. Image hosts need no auth (unverified). | `user.profile_photo.small` (160x160) and `.big` (640x640) as TDLib files, plus `minithumbnail` for an instant blur; `downloadFile`. `chat.photo` for groups. TDLib keeps its own file cache. | `{teams}/api/mt/{region}/beta/users/{mri}/profilepicturev2?size=HR64x64`, bearer token required (unverified). |
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
- Telegram: only what TDLib pushes for contacts and open chats; tinyclient sends nothing extra.

## Open questions for the owner

1. Discord risk. Member-list subscriptions (`op 14`) and profile fetches are what the official
   client does when you open a channel, but they end the README promise "does not scrape member
   lists, or track presence". Accept "only for what is on screen", and reword the README?
2. Should tinyclient ever send a Discord friend request or open a DM with a non-friend (story 13,
   and story 6 for strangers)? These are the actions Discord flags for user accounts. Default
   proposed: DMs yes, friend requests P2 behind a confirmation.
3. Does tinyclient show me as online while it runs in the background all day, or only while its
   window is focused? This decides what colleagues see on Slack and Teams.
4. Slack and Teams People default: only people I already talk to (proposed), or should the
   directory be browsable A to Z without typing?
5. Is one People list across networks right, or do you want Discord's friends screen to be its own
   place with the other three folded into search?
