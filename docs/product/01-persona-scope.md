# 01. Persona and the scope line

This replaces the persona and the "Cut" list in `docs/stories.md`. The other seven product docs fit inside the frame set here.

## What "full-fledged" means

**I can quit the official app for a normal day and nobody on the other side can tell.** The destination is every feature of the official clients, in a client that stays small and fast. What is not built yet opens in the official app with one click, on the exact message, channel, or meeting. A handoff that lands in the right place is how a missing feature behaves until it ships. A dead end is a bug.

## Personas

Each lists the daily loop and the things that send them back to the official app within a week.

**A. The owner.** Engineer, four networks all day, 18 GB Mac that is full. Slack and Teams for work (channels, threads, DMs, a few calls a day, one screen share), Discord for friends and dev communities, Telegram for a handful of people and groups. Leaves when: a work mention arrives with no notification; a screenshot shows as a filename; a call button does nothing; memory creeps toward what one Electron app cost.

**B. The Discord regular.** Discord is the social life: 10 to 30 servers, a friends list, a group DM, a voice channel most evenings. Checks who is online before messaging, drops into voice by clicking a channel, posts images, GIFs and links all day. Leaves when: no friends list or online dots; cannot see or join a voice channel; images, embeds, custom emoji and replies render as text; servers they are in are not listed.

**C. The work-chat employee.** One or two Slack workspaces or a Teams tenant, eight hours a day. Lives in channel threads, reactions as acknowledgement, pasted screenshots, huddles, and meeting links from the calendar. Leaves when: threads are missing or flattened; unread and mention state disagrees with their phone; cannot paste an image or attach a file; a meeting link does nothing; a second workspace cannot be added.

**D. The Telegram-first freelancer.** Clients and communities in Telegram: dozens of groups, broadcast channels, folders, voice messages, photos and documents both ways. Leaves when: voice messages do not play; photo albums and documents do not open; folders and pinned or muted chats are ignored so the list is noise; replies and forwards lose their context.

What they share: all four tolerate a missing *feature*; none tolerate a missed *message* or a message that looks broken. That sets the ranking below. Supporting evidence: users of the unified client Beeper call missing @mentions "a deal breaker" and cite unsupported Discord threads ([r/beeper](https://redlib.hbubli.cc/r/beeper/comments/1juae4c/the_missing_killerapp_function), [Manifold](https://manifold.markets/wasabipesto/will-i-be-able-to-participate-in-a)); the native Slack and Discord client Ripcord ran in 35 to 50 MB and was liked for it, but users describe it as lacking the official clients' features and it stopped shipping in 2021 ([AlternativeTo](https://www.alternativeto.net/software/ripcord/about/)).

## Table stakes, ranked

Ranked by how fast its absence sends someone back. P0 = not a real client without it. P1 = expected within the first month. P2 = nice, after everything else.

| # | Capability | Pri | Why it ranks here |
| --- | --- | --- | --- |
| 1 | Everything I am in is listed: servers and their channels, Slack channels and group DMs, Telegram groups and channels, Teams chats and teams | P0 | Today only DMs and mentioned channels exist. Most of B's and C's day is invisible. |
| 2 | Unread, mention and read state are right and sync both ways; native notifications and dock badge; mutes honored | P0 | A missed message is the one unforgivable failure. |
| 3 | Messages look as sent: inline images, files that open, link previews, mentions, emoji and custom emoji, formatting, code blocks, reply quotes, edits and deletions applied | P0 | Owner: "I can't see images and stuff". A filename is a broken message. |
| 4 | People look like people: real avatars, display names, presence dot | P0 | Owner: "I don't see the user's avatar", "status indicators". |
| 5 | Friends and people: Discord friends list, Slack and Teams people, Telegram contacts; start a new DM | P0 | Owner: "no way to see my friends list". Today you cannot start a conversation. |
| 6 | Threads: Slack and Teams channel replies, Discord threads and forum posts, Telegram reply chains | P0 | Work chat happens in threads. Flattened threads are unreadable. |
| 7 | A real composer: multi-line, paste or drop an image, attach a file, @mention completion, emoji, edit and delete my own message, reply to a message | P0 | Sending one line of plain text is a demo. |
| 8 | Reactions: see them, add and remove them | P0 | Reactions are how work chat says "seen" and "yes". |
| 9 | Scroll back through history; jump to a conversation by name (Cmd-K) | P0 | 50 messages and a filter field are not enough for a day. |
| 10 | Typing indicators, shown and sent | P0 | Owner asked. Cheap, and its absence reads as "dead". |
| 11 | Voice that works: Discord DM calls and voice channels, Slack huddles, Telegram calls; mute, device choice, incoming ring | P0 | Owner: "I call and that's not wired up". B's whole evening. |
| 12 | Watch one screen share, send one screen share | P1 | The owner's stated need. One surface, never a grid. |
| 13 | Message search within a network | P1 | Daily for C, weekly for the others. |
| 14 | Telegram and Teams at the same depth as Discord and Slack | P1 | The four-network promise. Depth on two before breadth on four. |
| 15 | Voice messages play; stickers and GIFs display; pins; Telegram folders; set my own status | P1 | Expected by B and D, not on the critical path. |
| 16 | GIF and sticker pickers, record a voice message, vote in a poll, scheduled send, saved items | P2 | Authoring extras. Seeing them is P1, making them is P2. |

## Stories

Checks are observable from outside the process, as in `docs/stories.md`.

1. **P0. I see every place I belong.** I open tinyclient and every server, channel, group and DM I can see in the official app is reachable in at most two clicks. *Check:* for a test account, the set of conversation ids chatd can list equals the set the official client shows, per network.
2. **P0. I never miss a message.** A DM or mention arrives while tinyclient is in the background and I get a macOS notification; reading it on my phone clears it here, and the reverse. *Check:* a message from a second account produces one notification and unread 1; marking read on another device brings unread to 0 without touching tinyclient.
3. **P0. A message looks like what was sent.** An image shows as an image, a file opens, a link is clickable, an edit replaces the text, a deletion removes it. *Check:* a fixture conversation containing each content type renders with zero rows that are a bare filename or raw markup.
4. **P0. I know who I am talking to and whether they are around.** Every row and message carries the person's real avatar and name; friends and DM partners carry a presence dot. *Check:* a second account switching to idle changes its dot within 10 s; no initials placeholder remains once the avatar has loaded.
5. **P0. I can start a conversation.** I open the people view, pick a friend or coworker, and message them without having talked before. *Check:* a send to a user with no prior conversation creates one and delivers.
6. **P0. I can follow and answer a thread.** *Check:* a reply sent from tinyclient lands inside the thread in the official app, not in the channel.
7. **P0. I can answer the way I would in the official app.** Paste a screenshot, mention someone, react, fix a typo. *Check:* each of paste-image, mention, reaction, edit, delete is visible from a second account in the official client.
8. **P0. I can talk.** I click call or a voice channel and hear and am heard; when I leave, the media process is gone. *Check:* two-way audio with a second account; no `mediad` after leaving; chatd's memory is unchanged across the call.
9. **P0. It stays small while doing all this.** *Check:* the budgets in the performance doc hold with all networks signed in and a media-heavy channel open. This story gates every other one: a feature that breaks it is not done.
10. **P1. I can watch or give one screen share.** *Check:* as story 5 in `docs/stories.md`.
11. **P1. All four networks work the same way.** *Check:* stories 1 to 7 pass on Telegram and Teams.
12. **Cut, handed off. I can get to what tinyclient does not do.** Every message, channel and meeting has "Open in Slack/Discord/Telegram/Teams". *Check:* the link opens the official app on that exact item.

## Not built yet

These are part of the goal. They are not in the product yet, so each one opens in the official app. A feature ships when it passes the budgets in `07-budgets-and-risk.md`. A heavy part runs only while it is in use.

| Not yet | What has to be true when it ships | Until then |
| --- | --- | --- |
| Admin and moderation: server and workspace settings, roles, permissions, bans, audit logs, channel creation settings, invites management | Native screens. The calls are high-stakes, so they go through the same account-safety rules as everything else | Open in official app |
| App platforms: Slack app directory and workflows, Teams apps and tabs, Discord activities, Telegram mini apps and payments | A webview in its own process, running only while that app is open | Open in official app |
| Bot interactivity: buttons, menus, modals, slash-command forms | Native controls for that network's components. The message's text, images, and links already render | Open in official app |
| Documents inside chat: Slack canvases and lists, Teams Loop, whiteboards, Planner, the SharePoint file browser | A viewer or editor that is running only while it is open | A link card that opens the browser |
| Teams meetings and anything with a lobby, breakout rooms, captions, recording; Discord stage channels; Slack huddle extras (drawing, threads, reactions) | A join that works, with the media in `mediad` for the length of the meeting | The meeting link opens the official app |
| Video grid, virtual backgrounds, noise-suppression models, soundboard | Only the visible tiles decoded, in `mediad`, for the length of the call | One speaker and one share |
| Commerce and growth: Nitro, shop, quests, server discovery, boosts, Telegram Premium and stars, stories | The same budgets as the rest of the client | Open in official app |
| Account management: sign-up, password, 2FA, billing, privacy settings, per-channel notification rules | tinyclient already reads and honors the result (mutes, folders). Changing the account itself is still to build | Open in official app |
| Telegram secret chats | A second device cannot see them. The row says so | Say so in the row |
| Automation of any kind: auto-reply, scheduled bulk send, logging other users' presence, exporting member lists | This is what gets accounts banned. Discord's enforcement is aimed at self-bots and scraping ([Discord](https://support.discord.com/hc/en-us/articles/360039598252)) | Never |

**Guardrail for presence and member lists.** The owner's request overrides the old cut, and the README line "does not ... scrape member lists, or track presence" must change with it. The new rule: tinyclient asks the network only for what the official client would ask for the screen I am looking at (friends, open DMs, the member list of the channel that is open, paged as it scrolls) and stores presence in memory only, never in SQLite. No request exists whose purpose is to enumerate a server.

## Release order

Each increment is usable on its own. Discord and Slack first, because they are signed in today.

1. **Looks real.** Avatars and names everywhere; inline images, files, links, formatting, mentions, emoji; edits, deletions and reactions shown; scrollback; multi-line composer. *New:* I can read a normal conversation without opening the official app to see what someone posted.
2. **Everything and everyone.** Servers, channels and group DMs listed; friends and people view; presence; typing; start a DM; Cmd-K; correct unread and mention state; notifications and dock badge. *New:* I can leave the official app closed all day for text and not miss anything.
3. **Answer in kind.** Threads; add reactions; reply, edit, delete; paste and attach; @mention completion; emoji picker; search. *New:* nobody on the other side can tell which client I am using.
4. **Talk.** Audio for Discord DMs and voice channels and Slack huddles, with ring, mute and device choice; then one screen share each way. Built in `mediad` on a parallel track; ships when ready, not after 3. *New:* I can take a call without launching anything else.
5. **Four networks.** Telegram, then Teams chat, to the level of increments 1 to 3; Telegram calls; Teams meetings by handoff. *New:* the Slack, Discord, Telegram and Teams apps are all quit.

## Open questions for the owner

1. **Presence and member lists on Discord.** They require gateway subscriptions the README currently promises tinyclient does not make. Is the guardrail above (only what the official client requests for the visible screen, nothing stored) the rule, with the README rewritten to match?
2. **More than one account per network.** Still cut, or in? Persona C usually has two or more Slack workspaces, and Teams users often have guest tenants. Saying yes changes the store, the rail and sign-in now; adding it later is a migration.
3. **Teams meetings.** Handoff is how a meeting behaves until a join works inside `mediad` for the length of the meeting. It is not the permanent answer.
4. **Depth or breadth first.** The order above finishes Discord and Slack before Telegram and Teams. If Teams is where your own workday is, should Teams chat move ahead of increment 3?
5. **"Anyone who wants it".** Does that mean a signed, notarized app that starts chatd itself and updates itself, for people who will never open a terminal? If yes, packaging and first-run become an increment of their own, before 5.
