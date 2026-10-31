# 04 · Live signals

Typing, read state, counts, notifications, send state, connection state. "Unverified" marks a
protocol detail taken from memory or a single third-party source, not checked against a live session.

## Where the code is (main, 0.2.35)

- A row carries `mentions`, `unread`, and `muted` separately. A channel with unread and no mention is a dot. A mention is a number. Muted draws dim and does not notify.
- Read is `MarkRead`, and only when `may_mark_read` is true: the window is active, the conversation is open, and the message is on screen. Opening a conversation does not mark it read. A screenshot build never marks read.
- `Link::Expired` is not `SignedOut`. The cache stays readable. Send is refused until the person signs in again.
- Send returns immediately with `SendState::Pending` and a `client_id`. Failure is `Failed { reason }` with retry. If the network is down, the row says "Waiting for {network}…" and is given up after 60 seconds. It is not sent hours later on its own.
- Notifications are posted by chatd through the `notifier` crate, including while the window is closed: WinRT toasts on Windows, `UNUserNotificationCenter` inside the macOS bundle, and `org.freedesktop.Notifications` on Linux. A bare `osascript` fallback is not the design. Counts and the badge follow the notification settings in `09-settings.md`.
- Typing for the open conversation is drawn. Whether this client sends typing is Settings › Privacy, per account. Discord defaults off. Telegram cannot be turned off (the terms). Signal and Messages say the phone decides.
- Discord `VIEW_CHANNEL` is computed. Mute and unread follow the visible tree.

The story tables below are the checks for that behavior. They are not a backlog.

The macOS spike (a second binary in `Contents/MacOS`, launchd, who receives the click) is history. Do not treat it as work left to do. Ring delivery, in-call cues, and what a call does with media are `06-calls.md`. The settings that choose them are `09-settings.md`.

## Stories

### Typing
| # | P | Story | Check |
| --- | --- | --- | --- |
| T1 | P0 | I see "Ana is typing…" under the messages of the open conversation, within a second, with names for up to two people and "Several people are typing" past that. | Fake adapter emits typing; a new `typing` field for that conversation lists the user; shell shows the line. |
| T2 | P0 | The line goes away when their message arrives, when they stop, or after the network's timeout (table below), never later. | Emit typing, send nothing: gone at timeout. Emit typing then a message from the same user: gone at once. |
| T3 | P1 | In the list, a direct message row shows "typing…" in place of its preview. Channels do not (too noisy). | Typing in a DM changes that row only; typing in a channel changes no row. |
| T4 | P1 | Others see me typing while I type. I can turn this off per account in Settings › Privacy. Discord defaults off. Telegram cannot be turned off. Signal and Messages say the phone decides. | With it on, the adapter sends one typing signal per interval while the draft changes, and none once the draft is empty or sent. With it off, zero. |
| T5 | P2 | In a thread, typing shows only in that thread. | Depends on threads (area 02). |

Typing is not stored and must not bump `version` in a way that makes the shell refetch the list.

### Read state
| # | P | Story | Check |
| --- | --- | --- | --- |
| R1 | P0 | A conversation is read only when the window is key, the conversation is open, and the newest message is on screen. Otherwise it stays unread here and on my phone. | With the window in the background, a new message in the selected conversation raises its count and sends no ack. Bringing the window forward sends one ack. |
| R2 | P0 | If I read it on my phone, the count here clears within seconds, and its notification is withdrawn. | Inject the network's "read elsewhere" event; `list` shows 0; the delivered notification is removed. |
| R3 | P0 | Scrolled up in an open conversation, new messages show a "3 new" pill and a divider at the first unread; reading to the bottom marks read. | Count stays until the bottom is reached. |
| R4 | P1 | I can mark a conversation unread, and mark everything read for a network (Shift-Esc). | Ack to an older message id; the phone shows it unread. |
| R5 | P1 | On Telegram and Teams, my sent messages show sent and read marks, in direct chats. | The Telegram read-outbox update / Teams horizon moves the mark. Telegram here is grammers. |
| R6 | P2 | "Don't send read receipts" on Telegram and Teams. Honest label: the chat also stays unread on my other devices, because on Telegram the read position and the receipt are the same call. | With it on, no `viewMessages` / horizon PUT is sent. |

### Counts
| # | P | Story | Check |
| --- | --- | --- | --- |
| C1 | P0 | A row shows a **number** when someone needs me: every unread message in a DM or group DM, and each mention of me in a channel. It shows a **dot** when a channel has unread messages and none mention me. | `Conversation` carries `mentions`, `unread` and `muted` separately; a channel message without a mention gives a dot, not "1". |
| C2 | P0 | What I muted in the official apps is muted here, with no setup. A muted conversation shows no dot, no number, sorts as read, and never notifies. | Fixture READY / prefs with a muted channel: message arrives, row unchanged, no notification. |
| C3 | P0 | `@everyone`, `@here`, `@channel` and role mentions count as mentions only where my settings on that network say so (Discord `suppress_everyone` / `suppress_roles`; Slack per-channel prefs). | Fixture with `suppress_everyone: true`: no number. |
| C4 | P0 | The rail number per network is the sum of row numbers. Dots do not add up to a number. | Arithmetic over `list`. |
| C5 | P1 | Changing mute in the official app takes effect here without a restart. | Inject `USER_GUILD_SETTINGS_UPDATE` / `pref_change`. |
| C6 | P1 | I can mute and unmute from tinyclient and the official app agrees. | Round trip against the network. |
| C7 | P1 | A mention in a muted channel shows a quiet number and no notification (what Slack and Discord both do). | Row has a number; nothing posted. |

### Notifications, badge, sounds
| # | P | Story | Check |
| --- | --- | --- | --- |
| N1 | P0 | I get a macOS notification for exactly the things that get a number in C1, with the shell closed. Title: sender. Subtitle: "#channel · Server" or nothing for a DM. Body: the message as plain text. | Quit the shell, receive a DM: banner appears from "tinyclient" with those fields. |
| N2 | P0 | No notification when the conversation is open in the key window, for my own messages, for muted things, or for history loaded at connect or reconnect. | Each case posts nothing. After a reconnect with 20 missed messages, at most one summary per conversation. |
| N3 | P0 | Clicking a notification opens tinyclient on that conversation, starting the shell if needed. | Click with the shell closed: window opens on the right conversation at the bottom. |
| N4 | P0 | The Dock badge is the total of all numbers (C4). Nothing when zero. It is right with the shell closed. | Set through `UNUserNotificationCenter.setBadgeCount` from chatd (unverified with the app not running; otherwise the badge is only right while the shell is up). |
| N5 | P0 | Focus and Do Not Disturb are honored because the system delivers the notification, and the network's own DND (Slack snooze, Discord status "Do Not Disturb") silences that network. | Slack `dnd` active: number rises, no banner. |
| N6 | P1 | Notifications stack per conversation, and a burst from one person collapses into one that updates. | `threadIdentifier` = network + conversation id; five quick messages leave one banner. |
| N7 | P1 | I can reply from the notification. | Reply text goes out through chatd without opening the window; a failure posts "Not sent" that opens the draft. |
| N8 | P1 | One short sound for a notification, none while a call is up, and a "hide message text" setting for screen sharing. Global pause for 1 h / until tomorrow in the rail. | Settings change what is posted. |
| N9 | P2 | Per-conversation override inside tinyclient (all / mentions / nothing) that is written back to the network. | Same as C6. |
| — | cut | Custom sounds per network, keyword alerts managed in tinyclient (we honor Slack's keyword list if the server already counts it as a mention), time-sensitive breakthrough, avatar images in notifications in v1. | |

### Send state
| # | P | Story | Check |
| --- | --- | --- | --- |
| S1 | P0 | My message appears in the conversation the instant I press Return, slightly dimmed, and the composer is empty. It turns solid when the network accepts it. No duplicate when the echo arrives. | `Send` answers at once with a pending message carrying a client id; the adapter sends that id as nonce and the echo replaces the pending row. |
| S2 | P0 | If it fails, the row stays with "Not sent · Retry · Delete" and the reason in words ("Slow mode: 8 s", "You can't post here"). Nothing is lost on restart. | Force 403 / 429 / timeout; pending rows are in SQLite. |
| S3 | P0 | Offline: I can still press Return. The row says "Waiting for Slack…" and goes out if the network returns within 60 s; after that it becomes "Not sent" and waits for me. tinyclient never sends an old message on its own hours later. | Drop the link, send, restore at 30 s and at 90 s. |
| S4 | P1 | Rate limits are retried once after the delay the network gives, silently. | 429 with `retry_after`. |

### Connection state
| # | P | Story | Check |
| --- | --- | --- | --- |
| K1 | P0 | When one network drops, its rail dot goes hollow and, after 5 s (no flicker for a blip), the open conversation shows one line: "Discord is reconnecting…". Other networks are unaffected. | Kill one socket: only that network changes. |
| K2 | P0 | On reconnect the open conversation fills the gap, counts come from the network again, stale typing lines are gone, and nothing replays as a notification. | Messages sent during the gap appear in order. |
| K3 | P0 | A session the network ended is its own state: "Slack signed you out. Sign in again." with the button, my cached messages still readable, and one notification if the shell is closed. | `Link::Expired` distinct from `SignedOut`; Discord close 4004 / HTTP 401, Slack `invalid_auth` / `token_revoked`. |
| K4 | P1 | With the machine offline, one line says "No internet connection" instead of one error per network, and retry is immediate when the network path returns. | Path monitor, not only backoff. |
| K5 | P1 | Waking from sleep reconnects in under 3 s. | Wake notification resets the backoff. |

## What each network gives a user session

| | Discord | Slack | Telegram (schema; grammers in this client) | Teams |
| --- | --- | --- | --- | --- |
| Typing in | `TYPING_START` `{channel_id,user_id,timestamp}`. Always for DMs. Server channels only after subscribing to that guild with `typing: true` (op 14 / 37); subscribe for the open channel only. | `user_typing` `{channel,user}` on the socket we already hold. | `updateChatAction` (`chatActionTyping`, `chatActionCancel`). | Message with `messagetype: Control/Typing` / `Control/ClearTyping` on the event stream. |
| Expiry | 10 s (official client) | ~5 s (unverified) | ~5 s; the session sends the cancel | ~8 s (unverified) |
| Typing out | `POST /channels/{id}/typing` → 204, repeat every ~8 s (interval unverified) | `{"id":n,"type":"typing","channel":…}` on the socket, at most one per 3 s (documented for RTM; unverified on the web-client socket) | `sendChatAction`, every 5 s | `POST …/conversations/{id}/messages` with `Control/Typing` |
| Mark read | `POST /channels/{c}/messages/{m}/ack` (built) | `conversations.mark` (built) | `viewMessages` | `PUT …/conversations/{id}/properties?name=consumptionhorizon`, body `"{id};{ms};{clientId}"` |
| Read elsewhere | `MESSAGE_ACK` (fields unverified) | `channel_marked`, `im_marked`, `group_marked`, `mpim_marked` (unverified on this socket) | `updateChatReadInbox` | horizon in conversation properties / thread update (unverified) |
| Receipts others see | None | None | Yes, always, in the protocol. Only the read *time* can be hidden (`readDatePrivacySettings`). | Yes, chats up to 20 people; user and tenant setting. Whether the server enforces "off" is unverified. |
| Their receipts | None | None | `updateChatReadOutbox` | `…/threads/{id}/consumptionhorizons` (unverified) |
| Counts | `READY.read_state` (`mention_count`, `last_message_id`); unread = channel `last_message_id` newer | `client.counts` (`has_unreads`, `mention_count`) | `chat.unread_count`, `unread_mention_count` | Computed from horizon vs last message (unverified) |
| Mute source | `READY.user_guild_settings`: `muted`, `mute_config.end_time`, `suppress_everyone`, `suppress_roles`, `message_notifications`, `channel_overrides[]`; `guild_id: null` is DMs. Update: `USER_GUILD_SETTINGS_UPDATE`. Plus the guild default level. | Prefs `muted_channels`, `all_notifications_prefs` via `users.prefs.get`; `pref_change` event; `dnd.info`, `dnd_updated_user` (names unverified) | `chatNotificationSettings.mute_for`, `scopeNotificationSettings`; the notification-group update already applies them. Schema names, not a library we ship. | Conversation property `alerts` (format unverified) |
| Send ack / dedupe | `nonce` echoed in `MESSAGE_CREATE`; 429 `retry_after` | `client_msg_id` (unverified here); `ok:false` + `error` | Send succeeded / failed updates. Offline queueing is this client's 60-second window, not a second library. | `clientmessageid`; HTTP 201 |
| Signed out | Close 4004, HTTP 401 | `invalid_auth`, `token_revoked`, `account_inactive` | `authorizationStateLoggedOut` | HTTP 401 on token refresh |

Discord risk note: subscribing to guilds and sending typing are visible to Discord. Identify uses the web client's shape on API v9 (`07-budgets-and-risk.md`). Sending the real client's `capabilities` is what makes `read_state` and `user_guild_settings` arrive in the shape the docs describe.

Telegram in this client is grammers. TDLib names left in this table are the schema, not a library we ship. Signal, Messages, and Tiny are not in this table.

## Settled

Sending typing on Discord defaults off. That is Settings › Privacy, not an open question. Telegram typing and read receipts stay on. The Dock badge defaults to direct messages and mentions (N11 in `09-settings.md`). All unread is a setting. The system badge is a number. Packaging ships from GitHub releases (`08-sign-in-accounts.md`). An offline send waits 60 seconds, then becomes "Not sent". There is no open-at-login switch. Launching the window starts chatd.
