# Polish 02 · Slack lists

Product owner's rulings for what Slack's lists show and how they are organized. The shots were taken
on 2026-10-13 from `chatd --seed-demo` (Harbor Labs, Fernhill Collective). Owner's real shape: one
workspace, about 25 channels in his own sections ("Internal Chats", "Channels"), many private
channels, app DMs (GitHub, Linear, Google Calendar, Cloudflare), a few people. The target is also
5 workspaces x 200 channels. Evidence: **[C]** in our code, **[V]** Slack help pages, **[U]** unverified.

## 1. What reads as "thrown at you" today

| # | Shot | Finding |
|---|---|---|
| F1 | `--mode slack` | Slack home lists every workspace **expanded**, with all their sections, one after another. The second workspace starts below the fold. There is no overview and no way to stay inside one workspace. |
| F2 | `--mode space` | Threads are permanent tree rows ("↳ Thread in eng-platform") under their channel. Slack never puts a thread in the sidebar, the title says nothing, and the row never leaves (`tree.rs` `thread_known`). |
| F3 | real workspace | App and bot DMs (GitHub, Linear, Cloudflare Pages, Slackbot) sit among people at the same weight, in Direct messages and in the Inbox. `bot` is already known for every DM partner (`people.rs` `learn`, `is_bot`/`is_app_user`) and goes unused. |
| F4 | Inbox | Slack rows lead with the workspace: "H… › Mara Okafor, Dev…", "Fe… › Imogen Castellanos". With one workspace the prefix is pure noise; truncated, it is noise even with two. |
| F5 | `--mode space` | DMs come after every channel and are sorted by name (`load_tree` sorts by title). With 200 channels they are 200 rows down, and a DM from today ranks the same as one from 2023. |
| F6 | `--mode space` | A muted row carries a bell-slash glyph in the badge column, the same column as unread dots and counts. Three kinds of mark in one column, and the glyph repeats what the dimming already says. |
| F7 | DM rows | A DM row has no status emoji. Slack's sidebar shows it after the name, and it is the most-glanced Slack signal ("🌴", "🤒", "📅 in a meeting"). The data is already in `Person.status`. |
| F8 | Group DM | "Mara Okafor, Dev Patel" uses full names and a generic icon. With four people it truncates to nothing. |
| F9 | People | Slack colleagues are mixed in with Discord friends, and their subtitle is "Online". That repeats the presence dot and wastes the line that should carry title or status. |
| F10 | network ⋯ menu | The Slack "profile" menu has only Sign out per workspace. There is no status, pause notifications, away, or "open in Slack". |
| F11 | DM + info | The docked Profile shows empty Media, Files and Links tabs. Title and local time, the two things a work profile is for, are fetched (`profile()`) but sit below the fold or nowhere. The header title shrinks to "M… Ha…" because the "Open huddle in Slack" pill takes the room. |
| F12 | `--switcher` | An empty query shows nothing, against `05` §6, which asks for mentions and then recent places. Thread rows show in results as "Thread in eng-platform". |

## 2. Slack home: one workspace or several

- **R1. The Slack rail button opens a workspace, never a list of them.** It opens the last one used,
  which is the only one for the owner. That is Slack's own model: one workspace at a time.
- **R2. The workspace header is the switcher**: icon, name and a chevron. It opens a popover with each
  workspace (icon, name, mention pill or unread dot, link state) and `ctrl-1…5`, as in Slack. With one
  workspace, the chevron and popover are absent.
- **R3. Roll-up.** If another workspace has mentions, a small accent pill sits beside the chevron
  ("2" = mentions elsewhere); plain unread elsewhere shows a neutral dot. The rail's Slack count is
  mentions + DMs across all workspaces.
- **R4.** Pinning a workspace to the rail (N18) stays. A pinned workspace opens the same sidebar.
- *Cut:* the all-workspaces-expanded tree (F1).

## 3. A workspace's sidebar, top to bottom

1. **Header** (R2) with ⋯: Mark all read, Show (all / unreads only), Open in Slack.
2. **Threads**: a fixed row with a count of followed threads that have new replies, and an accent
   pill when one of them mentions me. See §4. This is the only built-in destination kept.
3. **The user's own sections, in Slack's order**, holding channels, DMs and apps exactly where the
   user filed them. Starred is a section like any other. Collapse is remembered (N5).
4. **Channels** (Slack's default section): whatever no custom section claims.
5. **Direct messages** (people and group DMs), **sorted by latest activity** [V: Slack's default is
   "channels alphabetically, DMs by recent activity"]. Only open DMs (`client.counts.ims`, as today).
6. **Apps**: app and bot DMs, plus Slackbot, which is `USLACKBOT` and not flagged `is_bot` [U].
   Collapsed by default. Its header carries an unread dot, never a count, and a pill only for a
   direct mention. Sorted by recency.

Rules:
- **R5. Sort within a section:** channels A to Z by name without the `#`, DMs and apps by recency.
  Slack's per-section sort override is honored if `channelSections` carries it [U], otherwise this default.
- **R6. Built-in groups.** Cut: Unreads view (the "unreads only" filter and the Inbox Unread chip
  cover it), Mentions & reactions (Inbox Mentions chip; reactions to me are cut), Activity tabs.
  Hand off with one ⋯ menu line "Open in Slack": Later/saved, Drafts & sent, Huddles list,
  Canvases, Directories. Directories become People search (§5).
- **R7. Show unreads only.** Honor Slack's sidebar pref and per-section overrides (all, unreads only,
  mentions only, active only) if `users.prefs.get` carries them [U]. Our own default when unknown:
  Slack's **"active only"**, which hides channels with nothing in 30 days, judged from
  `client.counts.latest` (already fetched, no call). Footer per section: "12 quiet channels" expands
  them. Open, starred and unread rows are never hidden.
- **R8. Muted channels** are dimmed and drop below the unmuted rows of their section (Slack's "hide
  muted" is honored if the pref is readable [U]). There is no glyph in the badge column (F6). The bell
  glyph appears on hover and in the header. A mention in a muted channel still shows a grey pill.
- **R9. Channel kinds by leading glyph only:** `#` public, lock private, two-org glyph for Slack
  Connect (`is_ext_shared`) [U: present on `users.conversations` objects], archived never listed.
  Prefixes (`eng-`, `team-`) are left whole. No grouping by prefix; that is the user's job, done in Slack.
- **R10. 200 channels** is 28 px x visible rows in a `uniform_list`, active-only (R7) and collapsible
  sections. The goal: the first screen holds Threads, the user's sections and DMs that changed today.

## 4. Threads as a destination

- **R11.** "Threads" (§3.2) opens a list in the sidebar, as Slack does. Each row: channel › first line
  of the root, latest replier avatars, reply count, time, and unread state. Enter opens the thread
  in the right pane with its channel as the conversation.
- **R12.** Thread rows leave the tree (F2). A thread is reachable from its message, the Threads
  list, the Inbox (N15, only when it mentions me or is a DM thread), and the switcher.
- What exists: `client.counts.threads` gives only `has_unreads`/`mention_count` for all threads
  together **[C]**; `subscriptions.thread.mark` and `thread_marked` work **[C]**. What is missing:
  the list itself. `subscriptions.thread.getView` (the web client's call) **[U]** is called once
  when Threads is opened, 20 per page. After that it is kept fresh by socket `message` events that
  have `thread_ts` for threads already listed. There is no polling. The row's count comes from
  `client.counts` until the view is opened.

## 5. People in Slack

- **R13. There are no friends.** Slack's People list holds the people I have an open DM or group DM
  with (`Relation::Contact`, as today), most recent first, in its own section per workspace.
  Everyone else is reached by **search only** (edge `users/search`, 25 results; never `users.list`).
- **R14. Bots and apps are never people.** They are not in People, not in member counts, and not in
  "Online". They are reachable in the workspace's Apps group (§3.6).
- **R15. Guests** (`is_restricted`/`is_ultra_restricted`) get a "Guest" tag; **external** people
  (profile `team` ≠ the workspace) get the other org's name as their tag; **deactivated** people
  (`deleted`) are dropped from People and shown greyed with "Deactivated" in old DMs. All three
  come from the same `users.info`/edge object **[U]** at no extra request; `learn` reads none today.
- **R16. Name rule.** Use the workspace's setting (`display_real_names`, team pref [U]): display name,
  else real name. A profile always shows both lines.

## 6. Row anatomy (fields in priority order; A = always, H = hover/selected, — = never)

| Row | Fields | Missing-field rule |
|---|---|---|
| Channel (28) | glyph `#`/lock/Connect (A) · name (A) · huddle-live glyph (A) [U: `huddle_state` on events] · mention pill or unread dot (A) · draft pencil (A, local draft only) · bell-slash (H) · topic (—, header only) | No glyph is known → `#`. |
| DM (28) | avatar 16 with presence (A) · display name (A) · status emoji (A, F7) · "you" for self-DM (A) · count (A) · status text, local time "3:40 PM, 9 h ahead" (H tooltip) | No emoji → nothing. Deactivated → greyed name, no presence. |
| Group DM (28) | two stacked avatars (A) · first names "Mara, Dev, +2" (A) · count (A) · full list (H) | Over 3 people → "+n". |
| App DM (28) | app icon (A) · app name (A) · dot only (A) · "App" tag (H) | No icon → initial in a square, not a circle. |
| Thread (48, Threads list) | channel name (A) · root's first line (A) · reply count + last 3 replier avatars (A) · time (A) · unread/mention (A) | Root deleted → "Original message deleted". |
| Inbox, Slack row (48) | title (A) · workspace name **only if more than one workspace is signed in**, after the title, never before it (F4) · preview · time · badge | One workspace → no prefix. |
| People, Slack (48) | avatar+presence (A) · name + status emoji (A) · line 2: status text, else title, else local time if ≥3 h away (A) · Guest / org tag (A) · DND moon (A, when known) | Never "Online" as text (F9). Nothing → one-line row. |
| Profile pane | name, display name, pronouns · status (emoji, text, until) · title · local time + offset · DND until · Message, Open in Slack · email, phone · shared channels (P2) · Media/Files/Links **only when non-empty** | Empty tab → hidden (F11). |

## 7. The profile button per service (Slack's ⋯ / own-avatar menu)

Per workspace: set status (emoji + text + clear after), **Pause notifications** for 30 min / 1 h /
until tomorrow (`dnd.setSnooze` already paced at tier 2), Active/Away, Open in Slack, Sign out.
Discord's "friends" and "requests" items are absent for Slack. They are different menus because
the networks differ, not one menu with greyed rows.

## 8. The right pane follows (owner's question)

- **R17. Docked Profile follows the conversation's subject.** Switching to another DM shows that
  person. A group DM shows its members. A channel shows channel details (topic, members, Connect
  org). Switching to Inbox or People leaves it showing the last subject until closed.
- **R18. A thread pane closes when I switch conversation**, unless I opened it from the Threads list,
  where moving through the list replaces it. *Departure:* Slack keeps an unrelated thread open
  beside a new channel [U]; the owner said "follow", and a thread from another channel beside this
  one is the "thrown at you" case.
- **R19. Member list** follows the channel and is hidden for a DM.

## 9. Slack-specific traps

- Unread ≠ mention ≠ thread reply. A channel's dot never counts thread replies (Slack doesn't).
  Threads have their own row and count (§4).
- `@here`/`@channel` count as mentions unless the channel's notification pref says otherwise (C3 of `04`).
- A DM with yourself is titled "Your name (you)" and pinned first in DMs, as Slack does.
- Huddle live on a channel: a glyph on the row and "Join in Slack" in the header. The header's big
  pill (F11) becomes an icon button when no huddle is running.
- Scheduled messages and Slack drafts: not shown (stories: no scheduled sends). A local draft
  marks the row with a pencil.
- App home tabs are cut. An app DM opens its Messages tab, and an "Open app in Slack" line replaces Home.

## 10. Cost of each decision (under `07`)

| Decision | Cost |
|---|---|
| Sections, DMs, apps, muted, active-only, sort | Free: `channelSections` + `users.conversations` + `client.counts` + `users.prefs.get`, once per tree load **[C]** |
| Bot/app split, guest, external, deactivated tags | Free: fields of user objects already fetched for DM partners **[C]** (store them; `learn` drops them) |
| Workspace roll-up | Free: per-workspace `client.counts` on connect, plus socket events |
| Threads list | One `subscriptions.thread.getView` call per open, paged **[U]**; never at connect |
| DND per person | `dnd.teamInfo` for **visible** DM partners only, batched, on view [U tier]; `dnd_updated_user` events after that |
| Huddle-live glyph | Only if socket events carry it [U]; never polled |
| Directory | Search only; `users.list` is forbidden (07 §5.4) |

## 11. Stories

| # | P | Story | Observable check |
|---|---|---|---|
| S1 | P0 | I click Slack and land in my workspace's sidebar, with no workspace row in between. | One workspace: rail click → header shows its name; no Space row in the list. |
| S2 | P0 | App DMs are in a collapsed Apps group, not among people. | GitHub/Linear/Slackbot DMs: `group = apps`; DMs list holds only humans; People has no bot. |
| S3 | P0 | My sections appear in Slack's order, and my DMs sort by latest activity. | Tree group order = `channelSections` chain; a new DM message moves that row to the top of DMs. |
| S4 | P0 | Threads never appear as tree rows. | `tree(space)` has no `ConversationKind::Thread` rows. |
| S5 | P0 | A Slack Inbox row has no workspace prefix when one workspace is signed in. | With one workspace, the title text equals the conversation title. |
| S6 | P0 | The docked profile follows when I move between DMs. | Open DM A, then the profile, then DM B: the pane shows B in the same frame. |
| S7 | P1 | I open Threads and see followed threads with new replies, newest first. | Row count = `getView` unread items; replying elsewhere bumps one row; opening one clears its count. |
| S8 | P1 | With several workspaces, I switch with the header or `ctrl-digit` and see mentions elsewhere. | A mention in workspace 2 shows a pill on workspace 1's header within 1 s of the event. |
| S9 | P1 | A DM row shows the person's status emoji, and a People row shows title or status, never "Online". | Set a status in Slack: the row's emoji updates on `user_change`. |
| S10 | P1 | Quiet channels fold away: "12 quiet channels" at a section's end. | A channel with `latest` older than 30 days is hidden unless unread, starred or open. |
| S11 | P1 | Guests, external people and deactivated people are told apart. | Fixture users with `is_restricted`, foreign `team` and `deleted` render the Guest tag, the org tag and greyed. |
| S12 | P1 | I pause notifications or set my status from Slack's menu. | `dnd.setSnooze` / `users.profile.set` are sent once; the menu shows "Paused until 3:40 PM". |
| S13 | P2 | Slack Connect channels show the other org; a live huddle shows on its row. | `is_ext_shared` → Connect glyph; huddle start event → glyph within 1 s. |
| S14 | P2 | Docked channel details show topic, member count and shared orgs. | Details pane fields equal `conversations.info`. |
| cut | — | Unreads view, Activity tabs, reactions-to-me, Later, Drafts & sent, Huddles list, Canvases, app home tabs, prefix grouping, all-workspaces-expanded home. | Each is an "Open in Slack" line where a user would look for it. |

## Open questions for the owner

1. **App DMs in the Inbox.** Recommended: kept out of the Inbox unless an app mentions me (they are
   notifications, not people). Or keep them in, de-weighted?
2. **Active-only default.** Hide channels quiet for 30 days (Slack's default), or show all 25 always?
3. **Thread pane on switch (R18).** Close it, as recommended, or keep Slack's behavior?
4. **Unread channels in the Inbox** (`05` q3) for Slack: recommended no. Your sections plus the
   unreads-only filter are where channel unread lives.

Sources: [Slack: Adjust your sidebar preferences](https://slack.com/intl/en-gb/help/articles/212596808-Adjust-your-sidebar-preferences),
[Slack: View all your unread messages](https://slack.com/help/articles/226410907),
[Slack: Introducing the new Activity view](https://slack.com/help/articles/46751260742035).
