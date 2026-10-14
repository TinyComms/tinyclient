# Stories

tinychat is a full client: I can quit the Slack, Discord, Telegram and Teams apps for a normal day
and nobody on the other side can tell. It is a complete participant, not an admin tool or an app
platform. The detail, with a check for every story, is in `docs/product/`:

| File | Covers |
|---|---|
| `01-persona-scope.md` | who it is for, table stakes, the cut line |
| `02-people.md` | friends and contacts, avatars, presence, profiles |
| `03-message-content.md` | images, files, formatting, reactions, replies, threads, the composer |
| `04-live-signals.md` | typing, read state, unread and mentions, notifications, send state |
| `05-navigation.md` | servers and channels, inbox, search, quick switcher, keyboard |
| `06-calls.md` | what each network's calls need and which are feasible |
| `07-budgets-and-risk.md` | memory, speed and idle budgets; rules that keep accounts safe |
| `08-sign-in-accounts.md` | first run, sign-in per network, accounts, settings, packaging |

## Persona

Justin, and anyone like him: one person, several networks, signed in all day on a machine that is
already full. Also the Discord regular (friends list, voice channels), the work-chat employee
(Slack or Teams threads and huddles) and the Telegram-first freelancer. They forgive a missing
feature. They do not forgive a missed message or one that looks broken.

## Rules that every story obeys

- No network's web UI is embedded. Sign-in is the one place a web page appears, in its own process.
- `chatd` never decodes media and never loads a media engine. Bytes go to disk; the socket carries
  paths. `mediad` exists only during a call.
- Lists I own (friends, contacts, DMs) are held whole. Servers and directories are searched or
  loaded by visible range, never downloaded. Presence is kept in memory for what is on screen.
- Anything tinychat cannot draw is one labelled line with a link to that item in the official app.
  Nothing is blank and nothing is silently dropped.
- The shell shows a call only after the network confirms the join. A call tinychat cannot carry
  opens in the official app.
- A conversation is read only when the window is active, it is open, and its newest message is on
  screen. Mute settings made in the official apps are honored.
- Nothing is automated: no scheduled sends, no friend requests, DMs or joins the user did not make.
- Whole product idle on four networks: 150 MB. Per-state budgets are in `07-budgets-and-risk.md`.

## Release order

Discord and Slack first, because they sign in today. Each increment is usable on its own.

1. **Looks real.** Avatars and names; inline images, files, links, formatting, mentions, emoji;
   edits, deletions and reactions shown; scroll back; multi-line composer.
   *I can read a normal conversation without opening the official app.*
2. **Everything and everyone.** Servers, channels and group DMs; People view with presence; typing;
   start a DM; Cmd-K; correct unread, mention and mute state; notifications and dock badge, which
   need `tinychat.app`. Several Slack workspaces under one Slack sign-in.
   *I can leave the official app closed all day for text and miss nothing.*
3. **Answer in kind.** Threads; add reactions; reply, edit, delete; paste and attach; mention
   completion; emoji picker; search.
   *Nobody can tell which client I am using.*
4. **Talk.** In `mediad`, on its own track. Discord voice first (DM calls and voice channels, with
   its end-to-end encryption), then one share each way; Slack huddles after a spike proves them.
   *I can take a call without launching anything else.*
5. **Four networks.** Telegram, then Teams chat, to the level of 1 to 3. Telegram 1:1 calls. Teams
   calls and meetings open in Teams. Work Teams accounts can be blocked by the tenant's own policy;
   nothing tinychat does changes that.

## Cut

Admin and moderation, app directories and bot UIs (buttons, slash commands), canvases and
whiteboards, a video gallery, Teams meetings, GIF picker, animated stickers, voice recording,
commerce, account settings of the networks themselves. Each opens in the official app.
