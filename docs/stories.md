# Stories

tinyclient is a full client for Discord, Slack, Telegram, Teams, Signal, Messages (Google Messages paired to this computer), and a Tiny server you run. A normal day of text should not need the official app. What is not built opens in that app, on the message or channel. Tiny has no official app to hand off to.

The detail, with a check for every story, is in `docs/product/`:

| File | Covers |
| --- | --- |
| `01-persona-scope.md` | who it is for, and what is still handed off |
| `02-people.md` | friends and contacts, avatars, presence, profiles |
| `03-message-content.md` | images, files, formatting, reactions, replies, threads, the composer |
| `04-live-signals.md` | typing, read state, unread and mentions, notifications, send state |
| `05-navigation.md` | servers and channels, inbox, search, quick switcher, keyboard |
| `06-calls.md` | calls, huddles, voice channels, screen share |
| `07-budgets-and-risk.md` | memory, speed, and idle budgets; rules that keep accounts safe |
| `08-sign-in-accounts.md` | first run, sign-in, accounts |
| `09-settings.md` | the settings window |

## Rules that every story obeys

- No network's web page is drawn inside the chat. Sign-in is a separate window, or the system browser for a Tiny server, or an isolated browser window for Messages.
- `chatd` does not decode media. Bytes go to disk. The socket carries paths. `mediad` runs for a call and for the short sounds and mic test that settings start.
- Lists the user owns (friends, contacts, direct conversations) are held whole. Servers and directories are searched, or loaded for the rows on screen. Presence is memory only. It is not a column on `person`.
- Anything tinyclient cannot draw is one labelled line and a link (`MessageKind::Unsupported`). Nothing is blank and nothing is dropped.
- The shell shows a call only after the network confirms the join. A call tinyclient cannot carry opens in the official app.
- A conversation is marked read only when the window is active, the conversation is open, and the newest message is on screen (`may_mark_read`). Mute settings from the official apps are honored.
- Nothing is automated: no scheduled sends, and no friend request, direct message, or join the user did not make.
- Whole product, idle, every signed-in network: 150 MB. Per-state budgets are in `07-budgets-and-risk.md`. That number is the gate, not a measurement from this commit.

## Calls

Discord voice, screen share, Slack huddles, Telegram 1:1 calls, and Teams meetings are `docs/product/06-calls.md`.

## Not built yet

These open in the official app until they ship. A Tiny server has its own settings screens for roles, members, bans, invites, channels, and the audit log; that does not extend to Discord, Slack, Teams, or Telegram admin.

- Admin and moderation on Discord, Slack, Teams, and Telegram.
- App directories, bot buttons, menus, and modals. The message's text, images, and links already render.
- Canvases, whiteboards, Loop, and the other in-chat documents.
- Teams meetings, and any call tinyclient does not carry (`06-calls.md`).
- Recording a voice message. Playing one in the message is already there.
- Commerce, discovery, and the networks' own account settings (password, 2FA, billing).
- Telegram secret chats. grammers does not carry them. The row should say a second device cannot see them.
