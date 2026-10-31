# Polish 03 · Telegram lists

Owner of Telegram inside tinyclient. Telegram is one flat chat list with folders as *filters*; it is
not servers with channels. **[C]** read in our code, **[V]** checked at source this pass (Telegram
API terms), **[U]** from memory of the official apps, confirm on the live account.

## 1. What a Telegram account produces today [C]

- **An invented Space.** `sync::sign_on` writes one Space "Telegram" (`SPACE = "chats"`). The rail's
  Telegram button opens a network home with *no* "Direct messages" (every row has `space` set, so
  `load_directs` filters all of them out) and one Space row "Telegram" with a letter tile, open,
  holding the groups: Pinned, one per folder, Chats, Archived. Rail-pinning that Space is possible
  and meaningless. The header subtitle reads "Telegram > Chats".
- **Folders as containers.** A chat in Work and Family shows under the first only; the rest of a
  folder looks empty. Folder rules `exclude_muted / exclude_read / exclude_archived` are not read,
  pins *inside* a folder are flattened, the folder's own order of chats is lost.
- **Pinned** is a group in the tree; the Inbox never hears of Telegram pins.
- **One-line 28 px tree rows**: no preview, time or face for groups and channels. A Telegram list is
  faces with last lines. A broadcast is `Channel`, so the Inbox says "Telegram › #Durov's Channel".
- **Everything in the Inbox** (`listed = true`): 300 channels and the archive flood the mixed list.
- Saved Messages shows my own photo and presence; the 777000 chat (login codes) gets a Call button.
- Avatars fetched for the first 40 chats only; chats past 500 (archive past 100) absent until active.
- **Dropped:** my ✓✓, unread reactions, cloud drafts, verified/scam/fake, per-folder unread,
  member counts, post views, signature, comments. A pin or folder change re-reads all 600 dialogs.

## 2. The model (decisions)

1. **No Space for Telegram.** The rail button opens **Telegram home**: the chat list, 48 px two-line
   rows (the Inbox row minus the network icon), Telegram pins in Telegram's order, then recency.
2. **Folder chips** under the title: `All`, then folders in Telegram's order, each counting *unread
   unmuted chats* (Telegram counts chats). Chips scroll sideways; choice remembered per account.
   Departure: Telegram Desktop also has a vertical folder column [U]; at 316 px, chips.
3. **A folder is a filter**, never a container: a chat shows in every folder it matches, the
   folder's own pins on top. `exclude_read` keeps the open chat in place until I leave it.
4. **Archive** is one collapsed 28 px row atop `All` (three newest names, quiet rollup); it drills in
   with a back chevron. Archived chats never enter the Inbox or the rail count unless they mention me.
5. **Inbox** takes Telegram people, bots, groups and the service chat, not archived; channels only
   when starred. Telegram pins are not Inbox pins.
6. **Forum** drills into two-line topic rows (pinned, then activity) and opens the newest unread
   topic, else General.
7. **Right pane follows the open chat.** Info and members re-target; a docked person profile follows
   to the new one-to-one, else becomes that chat's Info; a comment thread closes when its channel is
   left. The right pane never describes a chat I am not in.

## 3. Kinds of chat and their signals

| Kind | Avatar | Title mark | Preview | Composer | Call | Notes |
|---|---|---|---|---|---|---|
| Person | round photo, online dot only | verified ✓ / Premium none | text | yes | handoff | offline draws nothing in lists |
| Bot | round photo, no dot | "bot" tag | text | yes | none | buttons are a labelled line |
| Group / supergroup | round photo | none | "Sender: text" | yes | none | count of members in Info |
| Channel | round photo | megaphone glyph only when no photo | text, no sender | read-only unless I may post | none | sponsored, views, comments |
| Forum | round photo | topics glyph | "Topic › Sender: text" | per topic | none | drills into topics |
| Saved Messages | bookmark tile | none | text | yes | none | no presence, pinned by default [U] |
| Service 777000 | Telegram logo | verified ✓ | text, codes shown | read-only | none | never muted by tinyclient |
| Secret chat | n/a | | | | | not visible to this client at all |
| Scam / fake | photo | red "Scam" / "Fake" tag, always | | | | from the user/channel flags, free |

Secret chats are device-bound and never reach us. Rule: when Telegram tells us someone started one
(`updateEncryption`), a toast says "Ines started a secret chat. Open it on your phone." P2.

## 4. Row anatomy (data, priority order)

**Telegram home / Inbox row** (48 px). Missing field: its slot collapses, nothing placeholder-like.

| # | Field | Shown | Missing |
|---|---|---|---|
| 1 | Avatar (photo, else initials on tint; bookmark / logo for the two specials) | always | initials |
| 2 | Title: contact name as I saved it, else first + last, else @username, else "Deleted Account" | always | never empty |
| 3 | Kind/trust tag: ✓ verified, Scam, Fake, bot | always | none |
| 4 | Time of newest message | always | blank |
| 5 | Badge: `@` accent for unread mention; neutral count for unread; neutral dot for marked-unread | always | none |
| 6 | Mute: bell-off, count drawn quiet | always when muted | none |
| 7 | Telegram pin glyph | Telegram home only | none |
| 8 | Line 2: typing > draft ("Draft:") > "Sender: preview" (groups) / preview | always | "No messages yet" |
| 9 | ✓ / ✓✓ before my own last message (person and group chats) | always | none |
| 10 | Unread reaction heart | always when present (P2) | none |
| 11 | Online dot (people, not bots, not me) | always | nothing (unknown) |
| — | Phone number, member count, views, folder names | never in rows | |

**Topic row:** topic icon colour/emoji as a still, title, time, preview with sender, badge, closed lock.
**People row (contacts):** avatar + online dot, name, line 2 = "online" / "last seen 14:02" /
"last seen recently" (Telegram's own words, never invented), @username on hover. Phone never.

## 5. Contacts and People

- People for Telegram = **contacts** (held whole, one `GetContacts`). Chat partners who are not
  contacts are not People; they are chats. Search adds my chats, then global @username (on Enter).
- **Presence**: `online` → dot; `offline(at)` → no dot, "last seen at" on row 2 and profile;
  `recently / within a week / within a month / hidden` → `Unknown`, worded as Telegram words it.
  Never draw Telegram unknown as offline (terms: last seen must display correctly [V]).
- **Phone**: never in lists, search results or menus. Profile shows it only if Telegram returns it
  for that person; my own number on my card is masked until clicked.
- Profile fields: name, @username(s), bio, phone if shared, last seen wording, "mutual contact",
  groups in common count, birthday if shared (P2). Fetched once on open (`GetFullUser`), never per row.
- Blocked list P2. **Cut:** people nearby, address-book import, add-by-phone, stories.
- **My Telegram profile button:** name, @username, masked phone, "Online while open"; no picker.

## 6. Channels

- **Sponsored [V terms]:** a client that shows channels must show them. At the channel's foot when
  open, labelled "Sponsored" with sponsor info; never a preview, notification or Inbox row. Viewed
  when on screen, `ClickSponsoredMessage` when followed (both missing today; P0).
- **Comments** open as a thread in the right pane, count on the post. P1. Post footer: views
  ("12.4K"), signature, edited. Muted channels: dimmed, quiet count, never raise the rail.

## 7. Things multi-network clients get wrong

- Contact names are mine (Telegram returns my saved name); @username is secondary. Custom and
  Premium emoji draw their alt glyph (image P2, still). Stickers of every kind are stills.
- `ttl_period` (auto-delete): the message leaves our store at its time, offline too. P1.
- Scheduled messages: cut. Polls, games, invoices, giveaways, stories: labelled line [C].
- Read and typing are told as the official apps tell them; no toggle for Telegram (terms [V]).
- Multiple accounts P2: Telegram home gets an account chip, rows name the account.

## 8. Cheap and expensive

- Cheap: dialogs (100 a call with newest message, unread, mentions, reactions, mute, pin, draft),
  `GetDialogFilters`, `GetContacts`, `GetPinnedDialogs` (one call each), flags already received.
- Expensive: `GetFullUser`/`GetFullChannel` (Info open only), members (visible range), avatars
  (one file each: **visible rows**, small size, ≤4 in flight), history.
- Dialogs once at sign-on, first page at once, rest paced; then updates only. A folder or pin change
  is one call, never a re-read of every dialog. Flood waits slept in full [C].

## 9. Contract needs, in plain words

1. A network may have **no Spaces**: its home is a flat recency list of its conversations.
2. A conversation carries **its folders** (a set), **archived**, and **the network's pin** with its
   order in All and per folder, separate from tinyclient's local star.
3. A **folder** is id, title, order and rules (kinds, contacts, include/exclude, exclude
   muted/read/archived). chatd applies the live rules when it answers and returns folder counts.
4. A row carries last sender (groups), **read by them** on my last message, unread reactions, a
   **trust tag**, **bot**, a cloud draft; `Announcement` for broadcasts; a saved/service special.
5. The shell says **which rows are on screen**; the media cache can ask the adapter for a file.
6. A message can **expire**, and carry **views, signature, comment count**.

## 10. Stories

| # | P | Story | Check |
|---|---|---|---|
| T1 | P0 | I click Telegram and see my chat list as on my phone: pins, then recency, faces and last lines. | First 10 rows equal the official app's All list in order; no Space row, no "Telegram" group. |
| T2 | P0 | I tap a folder chip and see exactly that folder; a chat in two folders is in both. | Counts per chip equal the official app's; a chat in Work and Family appears under each. |
| T3 | P0 | Archived chats stay out of my way but one row away. | Archive row at top of All; no archived chat in the Inbox; mention in one raises the rail. |
| T4 | P0 | Channels read as channels: no `#`, sender-less preview, read-only, sponsored post at the foot. | Inbox has no channel unless starred; a channel shows one "Sponsored" item, never as preview. |
| T5 | P0 | My login code from Telegram arrives where I can see it. | 777000 row in the Inbox with ✓ and no Call button; its message notifies. |
| T6 | P0 | Every visible row has its picture, not only the first 40. | Scroll to row 200: avatar lands within 2 s; at most 4 fetches in flight. |
| T7 | P1 | Groups show who spoke last; my messages show ✓ then ✓✓ when read. | Preview "Mara: ok"; ✓✓ appears within 5 s of the other side reading. |
| T8 | P1 | A forum opens as its topics, newest unread first. | Topic rows have counts; opening the forum lands on the newest unread topic. |
| T9 | P1 | Contacts in People say "last seen recently" honestly. | Hidden-last-seen contact: no dot, Telegram's wording; online contact: dot within 5 s. |
| T10 | P1 | I switch chats with the Info pane open and it follows. | Info shows the new chat every time; a thread pane closes when its chat is left. |
| T11 | P1 | Scam/fake/verified marks show wherever a name does. | Seeded flags render in row, header and profile. |
| T12 | P2 | Comments under a channel post open as a thread; unread reactions show; cloud drafts sync. | Comment count matches; draft typed on phone shows as "Draft:". |
| T13 | P2 | A second Telegram account, and a secret chat notice. | Two accounts listed with own unread; the toast appears once. |
| cut | | Scheduled sends, people nearby, stories, contact import, emoji statuses in rows, Premium features, mini apps. | Open in Telegram. |

## Open questions for the owner

1. May a **Telegram folder be pinned to the rail** like a Space (your "Work" folder one click away)?
   Proposed: yes, P2.
2. Should **broadcast channels** stay out of the cross-network Inbox unless starred (proposed), or
   is Telegram your news feed and they belong there?
3. Telegram **pins vs Inbox pins**: keep them separate (proposed), or seed Inbox stars from Telegram pins?
4. Read **all** dialogs at sign-on (true to "lists I own are held whole", ~20 paced calls for 2,000
   chats), or the newest 500 and the rest on demand?
