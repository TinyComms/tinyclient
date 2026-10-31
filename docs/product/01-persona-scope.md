# 01. Persona and the scope line

Personas and the cut line. The index is `docs/stories.md`. The other product docs fit inside the frame set here.

## What "full-fledged" means

**I can quit the official app for a normal day and nobody on the other side can tell.** The destination is every feature of the official clients, in a client that stays small and fast. What is not built yet opens in the official app with one click, on the exact message, channel, or meeting. A handoff that lands in the right place is how a missing feature behaves until it ships. A dead end is a bug.

## Personas

Each lists the daily loop and the things that send them back to the official app within a week.

**A. The owner.** Engineer, four networks all day, 18 GB Mac that is full. Slack and Teams for work (channels, threads, DMs, a few calls a day, one screen share), Discord for friends and dev communities, Telegram for a handful of people and groups. Leaves when: a work mention arrives with no notification; a screenshot shows as a filename; a call button does nothing; memory creeps toward what one Electron app cost.

**B. The Discord regular.** Discord is the social life: 10 to 30 servers, a friends list, a group DM, a voice channel most evenings. Checks who is online before messaging, drops into voice by clicking a channel, posts images, GIFs and links all day. Leaves when: no friends list or online dots; cannot see or join a voice channel; images, embeds, custom emoji and replies render as text; servers they are in are not listed.

**C. The work-chat employee.** One or two Slack workspaces or a Teams tenant, eight hours a day. Lives in channel threads, reactions as acknowledgement, pasted screenshots, huddles, and meeting links from the calendar. Leaves when: threads are missing or flattened; unread and mention state disagrees with their phone; cannot paste an image or attach a file; a meeting link does nothing; a second workspace cannot be added.

**D. The Telegram-first freelancer.** Clients and communities in Telegram: dozens of groups, broadcast channels, folders, voice messages, photos and documents both ways. Leaves when: voice messages do not play; photo albums and documents do not open; folders and pinned or muted chats are ignored so the list is noise; replies and forwards lose their context.

Signal, Messages, and a Tiny server are in the same client; they are not a fifth increment.

What they share: all four tolerate a missing *feature*; none tolerate a missed *message* or a message that looks broken. That sets the ranking below. Supporting evidence: users of the unified client Beeper call missing @mentions "a deal breaker" and cite unsupported Discord threads ([r/beeper](https://redlib.hbubli.cc/r/beeper/comments/1juae4c/the_missing_killerapp_function), [Manifold](https://manifold.markets/wasabipesto/will-i-be-able-to-participate-in-a)); the native Slack and Discord client Ripcord ran in 35 to 50 MB and was liked for it, but users describe it as lacking the official clients' features and it stopped shipping in 2021 ([AlternativeTo](https://www.alternativeto.net/software/ripcord/about/)).

## What is already a normal day

On `main` (0.2.35) the text client covers Discord, Slack, Telegram, Teams, Signal, Messages, and Tiny. The person can list spaces and channels, open People, start a direct conversation, read and send formatted messages with images and files, react, reply, follow a thread, search, and get a notification with the official app closed. Sign-in, accounts, and the settings window are `08` and `09`.

Calls, screen share, and what still hands off to the official app are `06-calls.md`.

## Table stakes, ranked

Ranked by how fast its absence sends someone back. P0 = not a real client without it. P1 = expected within the first month. P2 = nice, after everything else.

| # | Capability | Pri | Why it ranks here |
| --- | --- | --- | --- |
| 1 | Everything I am in is listed: servers and their channels, Slack channels and group DMs, Telegram groups and channels, Teams chats and teams | P0 | A day is the servers, channels, groups, and directs the person belongs to. |
| 2 | Unread, mention and read state are right and sync both ways; native notifications and dock badge; mutes honored | P0 | A missed message is the one unforgivable failure. |
| 3 | Messages look as sent: inline images, files that open, link previews, mentions, emoji and custom emoji, formatting, code blocks, reply quotes, edits and deletions applied | P0 | Owner: "I can't see images and stuff". A filename is a broken message. |
| 4 | People look like people: real avatars, display names, presence dot | P0 | Owner: "I don't see the user's avatar", "status indicators". |
| 5 | Friends and people: Discord friends list, Slack and Teams people, Telegram contacts; start a new DM | P0 | Friends, contacts, and starting a direct conversation. |
| 6 | Threads: Slack and Teams channel replies, Discord threads and forum posts, Telegram reply chains | P0 | Work chat happens in threads. Flattened threads are unreadable. |
| 7 | A real composer: multi-line, paste or drop an image, attach a file, @mention completion, emoji, edit and delete my own message, reply to a message | P0 | Paste, mention, edit, and delete are the composer. |
| 8 | Reactions: see them, add and remove them | P0 | Reactions are how work chat says "seen" and "yes". |
| 9 | Scroll back through history; jump to a conversation by name (Cmd-K) | P0 | 50 messages and a filter field are not enough for a day. |
| 10 | Typing indicators, shown and sent | P0 | Owner asked. Cheap, and its absence reads as "dead". |
| 11 | Voice that works: Discord DM calls and voice channels, Slack huddles, Telegram calls; mute, device choice, incoming ring | P0 | Owner: "I call and that's not wired up". B's whole evening. Owned by `06-calls.md`. |
| 12 | Watch one screen share, send one screen share | P1 | The owner's stated need. One surface, never a grid. Owned by `06-calls.md`. |
| 13 | Message search within a network | P1 | Daily for C, weekly for the others. |
| 14 | Seven networks at a usable depth | P1 | Seven networks sign in. Depth still differs; a gap is a handoff, not a hidden row. |
| 15 | Voice messages play; stickers and GIFs display; pins; Telegram folders; set my own status | P1 | Expected by B and D, not on the critical path. |
| 16 | Record a voice message, vote in a poll, scheduled send, saved items | P2 | The GIF and sticker picker is in. Recording a voice message, polls, scheduled send, and saved items are still authoring extras. |

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
10. **P1. I can watch or give one screen share.** *Check:* `06-calls.md`.
11. **P1. The signed-in networks work the same way.** *Check:* stories 1 to 7 pass on each network. Depth still differs; a gap is a handoff, not a hidden row.
12. **Cut, handed off. I can get to what tinyclient does not do.** Every message, channel and meeting has "Open in {network}" where that network has an official app. Tiny has no official app to hand off to. *Check:* the link opens the official app on that exact item.

## Not built yet

These are part of the goal. They are not in the product yet, so each one opens in the official app. A feature ships when it passes the budgets in `07-budgets-and-risk.md`. A heavy part runs only while it is in use.

| Not yet | Until then |
| --- | --- |
| Admin on Discord, Slack, Teams, and Telegram | Open in the official app. Tiny server settings are in the Tiny column. |
| Slack workflows, Teams tabs, Discord activities, Telegram mini apps | Open in the official app |
| Bot buttons, menus, modals, slash-command forms | The text, images, and links render. The controls open in the official app. |
| Canvases, lists, Loop, whiteboards | A link card |
| Teams meetings, stage channels, huddle extras | `06-calls.md` |
| Video grid, virtual backgrounds, soundboard | `06-calls.md` |
| Commerce, discovery, Premium, Nitro | Open in the official app |
| Password, 2FA, billing, and the network's own privacy screens | Open in the official app. Mutes, presence, and the local settings in `09` are here. |
| Telegram secret chats | grammers does not carry them. Say so on the row. |
| Scheduled send, recording a voice message | Not in the composer. Playing a voice message is in the message. |
| Light theme, open at login | Search says so. Dark only. No login item. |

A downloaded noise-suppression model is a non-goal. The Voice switch that turns suppression on is already a setting.

**Never.** Auto-reply, scheduled bulk send, logging other users' presence, and exporting member lists. This is what gets accounts banned. Discord's enforcement is aimed at self-bots and scraping ([Discord](https://support.discord.com/hc/en-us/articles/360039598252)).

**Guardrail for presence and member lists.** tinyclient asks only for the screen in front of the user: friends, open directs, and the visible range of the open channel's member list. Presence is not written to SQLite. There is no request whose job is to list a whole server.
