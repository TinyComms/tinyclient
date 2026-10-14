# Polish 04 · Microsoft Teams: what the lists show and how they are organized

Read 2026-10-13 from `adapters/teams*`, `gui/sidebar*`, `gui/people*` and shots of the shared frames
(the demo has no Teams account). **[U]** = not seen on a real tenant. Teams' own client (May 2025
"new chat and channels experience") has one Chat view: **Favorites**, **Chats** by recency, **Teams
and channels** in the user's order; filters Unread / Chats / Channels / Meeting chats; an @mentions
view; custom sections. tinyclient follows that shape, with the departures marked below.

## 1. What a Teams account looks like today, and what is wrong

Today: a Space "Chats" (one `Directs` group also called "Chats") plus one Space per team; the
Teams home has no chat section (`load_directs` drops rows with a Space) and lists Space rows only.

| # | Finding | Why a Teams user notices |
|---|---|---|
| F1 | Chats are a Space beside the teams. With 3+ teams the home has 4+ Spaces, so `space_open` collapses all of them, **Chats included**: the first view is a stack of closed rows. | Where they live is one click deeper than the teams they ignore. |
| F2 | Inside it, chats are 28 px tree rows: no preview, no time, inside a header "Chats" that repeats the Space name. | Teams' chat list is two lines with preview and time; this reads as a channel tree. |
| F3 | Meeting chats (`19:meeting_…`) are ordinary `Group` rows by recency. A worker has dozens; each meeting bumps it once and it then rots in the list. | The single biggest source of clutter in every Teams sidebar. |
| F4 | Self-chat (`48:notes`), the Activity feed (`48:notifications`) and call log (`48:calllogs`) are dropped as "system streams". | Teams users keep notes in the "(You)" chat; Activity is their inbox; missed calls vanish. |
| F5 | A 1:1 with a bot or app (`28:`) is never looked up (`lookup` takes `8:` only), so its title falls back to the last sender's `imDisplayName`, **which can be my own name**, or "Chat". | Workflows / Power Automate / Viva chats titled with my name. |
| F6 | Reply chains are rows titled "Replies in General", up to 5 under the channel, all with the same title. | Five identical lines under General. |
| F7 | Mentions that arrived while chatd was off are not known ("Mentions unread at connect are not told"); unread is a boolean shown as "1". | The Mentions chip, my Activity replacement, is empty after a restart. |
| F8 | Busy is drawn as do-not-disturb; a presence note hides "In a meeting"/"Out of office". | Teams' richest signal is flattened to a wrong glyph. |
| F9 | Channels show as `#General` (Teams never uses `#`), and two tenants give two Spaces both named "Chats"; a direct row only names its Space ("Chats ›"), never the tenant. | Guest tenants are indistinguishable. |
| F10 | Right pane stays on whatever it held (shot: "Thread 6 · No replies yet" over People and over an unrelated channel). | The pane does not follow what I select. |

## 2. Decisions: the Teams home (rail's Teams button)

1. **Chats are not a Space.** Chat rows get `space: None` like Discord DMs; the "Chats" Space, its
   group and the `CONTRACT` in `tree.rs` go. Free on the wire.
2. **One list in Teams' order:** `Pinned` (if any) → `Chats` → `Teams`. The home's direct section is
   titled per network ("Chats" on Teams). Pinned = Teams' own pins if the list carries them [U],
   else tinyclient's star; one section either way.
3. **Chats**: 48 px two-line rows (as the Inbox), by recency. Shown: unread, draft, or active in 7
   days, at least 8, then `Show n more`. Chats never collapse.
4. **Meetings**: a collapsed group closing Chats, holding meeting chats silent for 14 days; a new
   message brings one back. Departure: Teams mixes them in and offers a filter; this needs no setting.
5. **Teams**: Space rows in the user's order, collapsed by default at any count; expanded, the shown
   channels, General first. **Archived** teams sit under one collapsed `Archived` row, read-only.
6. **Several tenants** = several accounts: the home heads each with its tenant's name (`Fabrikam
   (guest)`); Inbox rows say `Fabrikam › Ana Ruiz` only when two tenants are signed in.

## 3. Decisions: chats

- **Naming.** 1:1: the person. Untitled group: first 2 names by most recent speaker, `, +n`
  ("Ana, Ben, +3"); never my own name; while names are unknown, "Group chat (5)". Titled: its title.
  Meeting chat: its subject. Self-chat (`48:notes`): "Notes", my avatar, by recency.
- **Bots and apps.** `28:` ids are looked up with `includeBots=true` (the call already allows it).
  A bot chat row shows the app's name + an `App` mark, is muted-looking (no presence), and sits by
  recency. No separate "apps" section: Workflows posts are often the work.
- **Hidden** (`hidden: true`, honored today): never shown; whether a new message unhides it as in
  Teams [U] is question 3. **Muted**: dimmed, no dot, mention pill stays (05 rule).

## 4. Decisions: teams and channels

- Show exactly the user's shown channels; hidden ones are reachable from `Show hidden (n)` at the
  end of the team (P2, local preview read-only). General first, then Teams' order.
- **Private** = lock; **shared** = a link glyph and the host team's name on hover [U: `channelType`
  2]. No `#` on Teams channels anywhere.
- **A channel is a list of posts:** subject, author, time, `n replies · last 2h · repliers`; unread
  replies make the summary accent; replies open in the right pane (N13).
- **Threads in the tree:** only chains I posted in, was mentioned in, or have open; max 3 under the
  channel; titled by the post (subject, else first 40 characters), never "Replies in …" (F6).
- **Announcements**: subject large, banner as an image. **Tabs, Files, Wiki, apps: cut**; the
  channel header carries `Open in Teams` (deep link exists).

## 5. Activity versus the Inbox

`Mentions` replaces Activity for @me, tags and replies in chains I posted in (already listed by
`message.rs`). Lost: mentions while offline (F7), missed calls, reactions to me, app alerts.
**Decision:** at connect read one page of `48:notifications` (Teams' own Activity stream [U shape])
for missed mentions, and `48:calllogs` for "Missed call · Ana Ruiz" rows. Reactions to me: cut.

## 6. People

- List = people I have a 1:1 chat with; the org is searched, never listed (02).
- **Presence, honest:** Busy, In a call/meeting, Presenting = a filled red dot that is *not* the DND
  bar: add `Presence::Busy` (one protocol variant). DND/Focusing = DND. Out of office is said in the
  status line, not a new colour. Away/BRB = Away.
- **Status line:** activity ("In a meeting") > OOF ("Out of office · until Fri") > note > title.
- **The right pane follows the selection** (F10): docked profile → the open 1:1's partner; a group →
  members; a channel → closes (rosters are team-sized) unless one of its threads is open.

## 7. Row anatomy (data, in priority order)

**Chat row (Teams home, Inbox)**

| Field | Shown | When missing |
|---|---|---|
| Avatar: person + presence corner; group: 2 stacked; meeting: calendar glyph; bot: app icon | always | initials of the title |
| Title (§3 naming) | always | "Group chat (n)" |
| Time of newest message | always | omit |
| Unread dot / mention pill / muted bell | always | nothing |
| Preview: `Name: text` in groups, `text` in 1:1, `You: …` mine, draft in accent "Draft: …" | always | "No messages yet" |
| Importance `!` (red, urgent) before the preview | when the newest unread is Important/Urgent | nothing |
| External / guest mark after the title | always | nothing |
| Tenant (`Fabrikam ›`) | only with 2+ tenants | nothing |

**Channel row** (28 px): lock/shared glyph · name · dot/pill · muted bell. **Post:** subject ·
author · time · reply summary · reactions. **Person row:** avatar+presence · name · `External`/
`Guest` · status line (§6); hover adds title · department; never email. **Profile card:** name,
presence word, note, OOF with dates, title, department, email, `Chat` / `Call in Teams` / `Open in
Teams`. Fields Teams lacks (banner, mutual servers, pronouns) are absent, never empty slots.

## 8. Things multi-network clients get wrong on Teams

- **Importance** (`properties.importance`, not read today): red `!`/"URGENT" on message and row;
  Teams repeats urgent alerts, tinyclient notifies once and keeps the row marked until read.
- **"Seen"** under my last message in 1:1 and small chats (04 R5); never in channels.
- **Cards, Loop, recaps, calls:** one labelled line + `Open in Teams` (Card: title · Loop component ·
  Meeting recording); `Event/Call` is a system row ("Call · 2 min", red "Missed call").
- **Reactions:** Teams takes any emoji since 2024 [U wire keys]; show all, send by Teams' key names.
- **Policy and expiry:** a 403 on send says "Your organization doesn't allow this here" in the
  composer; a blocked sign-in is the 08 banner once, no retry; expiry keeps rows readable from cache.

## 9. What is cheap and what is not

| Decision | Cost |
|---|---|
| Chats out of a Space, sections, meetings group, archived group | Free: all in the one `teams/users/me` answer already fetched |
| Chat and bot names; title, department | One `fetchShortProfile` per 100 unknown ids, once (cached); title/department are in `Known`, not yet in `PersonRow` |
| Presence incl. activity and note | Already in the batched `getpresence` (200 visible, ≤1/min) and push |
| OOF dates, work hours, time zone | Not in either call; Graph `mailboxSettings` is another audience: cut |
| `48:notifications`, `48:calllogs` | One page each at connect, like a warmed chat (07 rule 5) |

## 10. Stories

| # | P | Story | Observable check |
|---|---|---|---|
| T1 | P0 | I click Teams and see my chats first, two lines each, by recency, then my teams collapsed. | Home rows: label `Chats`, ≥8 chat rows with preview and time, then team rows; no "Chats" Space exists. |
| T2 | P0 | An untitled group chat is named after its people, never after me, never "Chat". | Fixture of 5-person untitled chat whose last message is mine: title "Ana, Ben, +3". |
| T3 | P0 | A Workflows or Power Automate chat is titled with the app's name. | `28:` partner chat titled by `fetchShortProfile`, row has `bot`. |
| T4 | P0 | Old meeting chats are out of the way but findable. | A meeting chat silent 15 days is under collapsed `Meetings`; a new message moves it to Chats; ⌘K finds both. |
| T5 | P0 | The right pane follows me: profile of the 1:1 partner, nothing stale. | Open chat B after A with profile docked: pane shows B's partner; open a channel: pane closes unless a thread of it is open. |
| T6 | P0 | Mentions I got while tinyclient was closed are in Mentions. | Mention while chatd stopped; start: Mentions count ≥1 from `48:notifications`. |
| T7 | P1 | A colleague in a meeting shows red and "In a meeting", not do-not-disturb. | `availability: Busy, activity: InAMeeting`: Busy shape, status "In a meeting" even with a note. |
| T8 | P1 | Threads under a channel are named by their post. | Chain rows titled by subject or first words; ≤3; never "Replies in". |
| T9 | P1 | I see which tenant a chat is in when I have two. | Two Teams accounts: home labels per tenant; Inbox rows say `Tenant ›`. |
| T10 | P1 | Urgent messages stand out. | `importance: urgent` gives a red `!` on row and message. |
| T11 | P1 | My Notes chat is there. | `48:notes` row "Notes", last in Chats; sending works. |
| T12 | P1 | Missed calls appear in the Inbox. | `48:calllogs` missed entry gives one Inbox row "Missed call · name". |
| T13 | P2 | I see and preview hidden channels. | `Show hidden (n)` lists `isFavorite:false` channels; open is read-only. |
| cut | | Tabs, Files, Wiki, apps; reactions-to-me feed; custom sections; work hours. | Open in Teams |

## Open questions for the owner

1. Old meeting chats: collapsed `Meetings` group after 14 silent days (recommended), or Teams' way
   (all by recency, with a filter)?
2. `Presence::Busy` as a fifth state in the protocol (recommended), or keep four and say "Busy" only
   in words?
3. Hidden chats: stay hidden in tinyclient even when a new message arrives, or unhide as Teams does?
4. Reading `48:notifications` (Teams' own Activity stream) to recover offline mentions: acceptable,
   given it is one more conversation the official client also reads?

Sources: [MC920179](https://mc.merill.net/message/MC920179), [Tarleton KB](https://services.tarleton.edu/TDClient/57/Portal/KB/Article/18675/Microsoft-Teams-New-Chat-and-Channels-Experience), [Graph presence](https://learn.microsoft.com/en-us/graph/api/resources/presence).
