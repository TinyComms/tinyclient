# 03. What is inside a conversation

Scope: what a message can contain, what I can do to it, and the composer. One model for Slack, Discord, Telegram and Teams. `(u)` marks a protocol detail not verified against a primary source; verify before building on it.

## Decisions

1. **One message model.** A message is: rich text (a string plus styled ranges and blocks), attachments, link previews, reactions, an optional reply-to, an optional thread summary, an optional forwarded-from card, and the flags edited, pinned, mentions-me. Adapters translate into it; the shell never sees Slack mrkdwn, Discord markdown, Telegram entities or Teams HTML.
2. **Nothing is ever blank.** Content tinyclient cannot draw (poll, location, contact, canvas, bot buttons, Teams card) shows as one labelled line, e.g. "Poll: Lunch? Open in Telegram", with a link into the real app. This rule is P0 and is what makes every later cut safe.
3. **Media lives on disk, and chatd fetches it.** chatd holds the sessions, so it streams bytes to `media/` under the support directory and never decodes them. The socket carries paths and sizes, never bytes (frames stay under 1 MiB). The shell decodes at display size only for rows on screen and drops the pixels when they scroll off.
4. **Never fetch an arbitrary URL.** tinyclient shows only the preview the network supplies, and loads preview images only through that network's own proxy or file store. If there is no proxied image, the card shows without one. Reasons: fetching a pasted URL tells a stranger's server my IP and that I read the message (tracking pixels work this way); it needs an HTML parser and a redirect and size policy in the always-on process; and all four networks already do the fetch server-side or sender-side.
5. **Images open in tinyclient; everything else opens in Quick Look.** One in-app image viewer (zoom, previous/next in the conversation, copy, save). Video, PDF, audio and documents download and open in the system Quick Look panel, which runs outside our process and costs no binary weight. "Open" hands off to the default app. No inline video player.
6. **Reply is inline, thread is a pane.** Two separate things in the model. A *reply* points at one earlier message and draws a one-line quote above the body; clicking it jumps there. A *thread* is a sub-conversation hanging off a root message; the root shows "N replies, last 2:14 PM" and opens in a pane on the right of the conversation (it replaces the conversation below 900 px width). Thread replies never appear in the main timeline unless the network also posted them there.
7. **Limits come from the session, not from constants.** Upload size and message length are read from the account or chat at sign-in and checked before sending.

## Content ranked by daily frequency

The order is my estimate from how the four networks are used, not measured data.

| # | Content | Priority | Note |
| --- | --- | --- | --- |
| 1 | Text with links | P0 | Links open in the default browser. |
| 2 | Mentions of people, channels, roles, everyone | P0 | Today these show as raw ids such as `<@U123>`. |
| 3 | Unicode emoji | P0 | System emoji font. Emoji-only messages draw larger (P1). |
| 4 | Reactions | P0 | Show and toggle. |
| 5 | Images and screenshots | P0 | The owner's complaint. |
| 6 | Reply / quote | P0 | |
| 7 | Bot and app messages (CI, alerts) | P0 text, cut buttons | Most of a work Slack channel. Draw their text, fields and images; no interactive controls. |
| 8 | Threads | P0 Slack and Teams, P1 Discord, P2 Telegram | |
| 9 | Formatting: bold, italic, strike, code, code block, quote, list | P0 | |
| 10 | Edits and deletes arriving live | P0 | A stale message is a wrong message. |
| 11 | Link previews | P1 | |
| 12 | Files (PDF, zip, docs) | P0 row and download, P1 Quick Look | |
| 13 | Custom emoji | P1 | P0 fallback is `:name:` as text. |
| 14 | GIFs | P1 | Still frame with a GIF badge, plays on click. |
| 15 | Video | P1 | Poster, duration, play badge; opens Quick Look. |
| 16 | Forwarded / shared messages | P1 | A nested card with the origin's name. |
| 17 | System lines (joined, call started, pinned) | P1 | One grey centered line. |
| 18 | Stickers | P1 static, cut animated | Lottie and WebM renderers are weight for little use. |
| 19 | Voice messages | P1 | Play button, waveform, duration. Needs an Ogg/Opus decoder; macOS does not play Ogg natively. |
| 20 | Spoilers | P1 | Hidden until clicked. |
| 21 | Pins | P2 | |
| 22 | Polls, location, contacts, tables, headings | P2 | Placeholder line until then. |
| — | Recording voice, GIF picker, slash commands, bot buttons and forms, canvases, scheduled send, edit history, read receipts per message | cut | |

## Stories

Each check is observable from outside the process: on the chat socket, on disk, or as process memory.

### P0

1. **I see the picture.** An image in a message shows as a thumbnail inside the message, at most 400 x 300 pt, never upscaled. *Check:* `open` returns the attachment with kind, name, bytes, width, height and a thumbnail path; the file at that path exists and is no larger than twice the display box.
2. **Nothing jumps while images load.** The row has its final height before any bytes arrive, filled with the network's blur placeholder where one exists. *Check:* every image attachment in `open` carries width and height; with the network cut after `open`, a screenshot shows boxes at final size and the scroll position does not change when it returns.
3. **Several pictures make a grid.** Two to four images tile; more show "+N" on the fourth. *Check:* a message with six images draws four tiles and the viewer steps through all six.
4. **I open a picture.** Click opens the viewer over the window; arrow keys move through the conversation's images, Esc closes, Cmd-C copies, Cmd-S saves to Downloads. *Check:* the full-size file appears in `media/` only after the click, and Cmd-S produces a byte-identical file in Downloads.
5. **Scrolling pictures does not cost memory.** *Check:* after scrolling through 200 images and back to text, the shell's footprint is within 10 MB of where it started; chatd's does not move by more than 1 MB.
6. **A file is a row I can act on.** Icon by type, name, size, and a download button with progress. Files never download on their own. *Check:* `media/` gains the file only after I ask; cancelling removes the partial file.
7. **Mentions read as names.** `@Priya`, `#deploys`, `@backend`, `@everyone` draw as pills; a message that mentions me has a tinted row. *Check:* `open` returns mention ranges with kind, id and label, and no body contains `<@`, `<#` or `<!`.
8. **Formatting looks the same on every network.** Bold, italic, strike, inline code, code block (monospace, horizontal scroll, copy button), quote bar, bulleted and numbered lists, links. *Check:* the same seeded sentence on all four networks returns the same ranges and blocks.
9. **I see and give reactions.** Chips under the message with emoji and count; mine is outlined; click toggles; a smiley on hover adds one. *Check:* a reaction added in the official app shows within 2 s without reopening; `react` on the socket changes the count in the official app.
10. **I reply to a message.** Reply from the hover bar puts "Replying to Priya" above the composer; the sent message carries the quote. *Check:* the official app shows it as a reply to that message; `open` returns reply-to with id, sender and snippet.
11. **I read and answer a thread.** The root shows the reply count; the pane shows the root and its replies with its own composer. *Check:* a Slack thread reply is absent from `open` on the channel, present in `thread_open`, and bumps the root's count live.
12. **Edits and deletes show up live.** An edited message changes in place and gains "(edited)"; a deleted one disappears. *Check:* edit then delete in the official app; `open` reflects each within 2 s and the cache row is updated then gone.
13. **What I missed while offline is corrected.** *Check:* edit a cached message while chatd is stopped; after restart, opening the conversation returns the new text.
14. **I edit and delete my own message.** Up-arrow in an empty composer edits my last message; Delete asks once. *Check:* the official app shows the change; if the network refuses (too old, no permission), its reason shows under the field and nothing changes locally.
15. **Bot messages are readable.** A CI or alert post shows its title, text, fields and image. *Check:* a Slack Block Kit message and a Discord bot embed each return non-empty text; neither shows as an empty row.
16. **Unknown content says what it is.** *Check:* a poll on each network returns a placeholder with a kind and a deep link; no message in `open` has an empty body and no parts.
17. **I write more than one line.** Enter sends, Shift-Enter adds a line, the field grows to eight lines then scrolls. *Check:* a three-line message arrives as three lines.
18. **I paste a screenshot.** Cmd-V with an image on the clipboard adds a thumbnail chip above the field; I can remove it; sending shows progress and can be cancelled. *Check:* the official app shows the image with my text as one message (Telegram: photo with caption).
19. **I attach by button or by dropping.** Cmd-U or the paperclip opens the file panel; dropping files on the conversation attaches them. A file over the limit is refused before upload with "Too big for Discord (limit 20 MB)". *Check:* an oversize file produces no network request.
20. **I mention someone.** Typing `@pr` lists matching people from this conversation, recent speakers first; Tab or Enter completes. *Check:* the recipient is notified as mentioned in the official app.
21. **A failed send loses nothing.** Text and attachments stay in the composer with the reason. *Check:* cut the network mid-upload; draft and chips remain.

### P1

22. **Links show their card.** Site, title, two lines of description, one image. *Check:* with a packet capture running, opening a conversation full of links contacts only the network's own hosts.
23. **GIFs play when I ask.** Still frame with a badge; click plays in place, stops when it leaves the screen or the window loses focus. *Check:* no CPU use from a visible conversation of GIFs that I have not clicked.
24. **Video and documents open in Quick Look.** Space or click on a downloaded file opens the panel. *Check:* no video decoder is linked into `tinyclient` or `chatd`.
25. **Custom emoji draw as images**, in text and as reactions, at text height. *Check:* a Slack `:partyparrot:` returns an emoji range with an image path.
26. **I pick an emoji.** `:tad` autocompletes Unicode and the workspace's custom emoji; a picker button opens a searchable grid. (P0 already has the macOS picker, Ctrl-Cmd-Space, for free.) *Check:* the sent emoji matches in the official app.
27. **Voice messages play.** *Check:* plays through the default output; one message at a time.
28. **Forwards, system lines, spoilers and static stickers draw** as described in the ranking. *Check:* each kind returns a typed part, not a placeholder.
29. **My draft survives.** Per conversation, across switching and restart. *Check:* type, quit, relaunch, reopen.
30. **I format as I type.** Cmd-B, Cmd-I, backticks and triple backticks; the adapter writes each network's own markup. *Check:* bold sent to each network shows bold in its official app.
31. **Discord threads and forum posts open in the pane.** A forum channel lists its posts as roots.

### P2

32. **I see and change pins.** A pin button in the header lists pinned messages; clicking one jumps to it; Pin and Unpin in the message menu.
33. **I see who reacted** on hover, fetched then.
34. **I drag an image or file out** to Finder or another app.
35. **Telegram comment threads, albums as one message, polls I can vote in, tables.**

## What each network gives a user session

| | Discord | Slack | Telegram (TDLib) | Teams |
| --- | --- | --- | --- | --- |
| Text markup | Markdown in `content`, parsed by the client: `**` `*` `__` `~~` `` ` `` ```` ``` ```` `>` `\|\|spoiler\|\|` `-` `1.` `#` `[t](u)` | `blocks` of type `rich_text` (structured; use this), else mrkdwn `text`: `*b*` `_i_` `~s~` `<url\|t>`. No spoiler. | `formattedText`: plain text plus entities by UTF-16 offset (bold, italic, underline, strike, spoiler, code, pre, blockQuote, textUrl, customEmoji). No lists. | HTML in `content`: `b i u s code pre blockquote ul ol a table`. Tokenize into the model; never render HTML. |
| Mentions | `<@id>` `<@&role>` `<#chan>` `@everyone`; the message carries `mentions` user objects, so no lookup | `<@U>` `<#C\|name>` `<!here>` `<!subteam^S>`; ids resolve through the existing name cache | `mention` (@username), `mentionName` (user id) | `<span itemtype=".../Mention">` plus `properties.mentions` with display names (u) |
| Image / file | `attachments[]`: `url`, `proxy_url`, `width`, `height`, `size`, `content_type`, `placeholder` (blur hash). Resize by adding `width`, `height`, `format` to the `media.discordapp.net` URL (params u). URLs are signed and expire (`ex`, `is`, `hm`); refetch the message or `POST /attachments/refresh-urls`. Key the disk cache by attachment id. | `files[]`: `thumb_360/480/720/960/1024` with `_w` `_h`, `thumb_tiny`, `url_private`, `mimetype`, `size`. Every fetch needs `Authorization: Bearer` and, for our `xoxc` token, the `d` cookie. `mode: hidden_by_limit` (free-plan history) and `file_access: check_file_info` need a placeholder or `files.info`. | `messagePhoto` sizes, `minithumbnail` (inline tiny JPEG), `messageDocument`, `messageVideo`, `messageAnimation`. `downloadFile(file_id, priority)` writes to TDLib's own directory and reports `updateFile` progress. | Inline `<img itemtype=".../AMSImage">` pointing at `*.asm.skype.com/v1/objects/{id}/views/imgo`, fetched with the skype token (u). Files are SharePoint or OneDrive links in `properties.files` and need a second token audience (u). |
| Video / GIF / voice | Video attachment has a poster through the proxy. Tenor and Giphy arrive as `gifv` embeds (MP4 plus thumbnail). Voice: message flag plus `duration_secs` and `waveform`, Ogg/Opus. | `thumb_video`, transcoded `mp4`. Clips are files with a transcript (u). | `messageVoiceNote` (Ogg/Opus, waveform), `messageVideoNote`, `messageAnimation` (MP4). | Giphy is an `<img>`; video is a file link. Voice messages (u). |
| Link preview | `embeds[]` built by Discord; images through `proxy_url`. Arrives in a later `MESSAGE_UPDATE`. | `attachments[]` built by Slack; arrives as `message_changed`. Image through Slack's image proxy (u), else no image. | `linkPreview` built by Telegram's servers; image is a Telegram file. | Card inserted by the sender's client (u). We read it; when sending we send the bare link. |
| Reactions | `reactions[]` on the message; `MESSAGE_REACTION_ADD/REMOVE/REMOVE_ALL`; `PUT .../reactions/{emoji}/@me` | `reactions[]` (`name`, `users`, `count`); `reaction_added/removed`; `reactions.add/remove`; images from `emoji.list` | `interaction_info.reactions`; `updateMessageInteractionInfo`; `addMessageReaction`; the chat limits which are allowed | `properties.emotions` (key plus users); set through the message properties endpoint (u) |
| Reply | `message_reference` plus `referenced_message` | none; a quote is a pasted link unfurl | `reply_to` with optional partial quote | `<blockquote itemtype=".../Reply">` in chats (u) |
| Thread | A thread is a child channel started from a message; forum posts are threads | `thread_ts`, `reply_count`, `latest_reply`; `conversations.replies`; `thread_broadcast` also lands in the channel | Forum topics are child conversations (sidebar's concern); channel comments hang off a post | In a channel every post is a root with a reply chain (u) |
| Edit / delete | `MESSAGE_UPDATE` is partial, so merge; `MESSAGE_DELETE`, `MESSAGE_DELETE_BULK`; `PATCH` and `DELETE` | `message_changed`, `message_deleted`; `chat.update`, `chat.delete`; workspace may cap the edit window | `updateMessageContent`, `updateMessageEdited`, `updateDeleteMessages` (ignore `from_cache`); `editMessageText`, `deleteMessages(revoke)` | Same message id re-sent with `edittime` or `deletetime` (u); `PUT` and `DELETE` on the message (u) |
| Pins | `pinned` flag, `CHANNEL_PINS_UPDATE`, pins endpoints | `pinned_to`, `pin_added/removed`, `pins.*` | `is_pinned`, `updateMessageIsPinned`, `pinChatMessage` | Pinned chat messages in thread properties (u) |
| Send a file | Ask `POST /channels/{id}/attachments` for an upload URL, `PUT` the bytes, name it in the message (user-client flow, u). 20 MB free as of Aug 2026 by one source (10 MB before), 50 MB Nitro Basic, 500 MB Nitro. | `files.getUploadURLExternal`, upload, `files.completeUploadExternal` (with `thread_ts`). `files.upload` is retired. | `sendMessage` with `inputMessagePhoto` or `inputMessageDocument` and a local path; 2 GB, 4 GB Premium | Pasted image: create an AMS object and reference it (u). File: upload to my OneDrive and share (u); P1. |
| Text limit | 2,000 (4,000 Nitro) | 40,000 | 4,096; caption 1,024 | about 28 KB of HTML (u) |

## Consequences for the rest of the product

- `protocol::Message` grows from a `body` string into the model in decision 1, and the socket gains thread open, react, edit, delete, download, and send-with-attachments. The store needs update and delete by message id, and a media table mapping attachment id to path, size and last use.
- The media cache is unencrypted like the message cache, capped (default 1 GB, least recently used goes first), and removed on sign-out with the rest of that network's data.
- Both adapters currently treat every `MESSAGE_UPDATE` / `message_changed` as noise. Link previews arrive through the same events, so P0 edits and P1 previews share one code path.
- Three risks to size before committing: an Ogg/Opus decoder for voice (small), playing MP4 "GIFs" in place without a general video player, and the Teams rows marked `(u)`, which are most of that column.

## Open questions for the owner

1. **Moving pictures.** Is "still frame, plays on click" acceptable for GIFs, and static-only for animated stickers? Autoplay and Lottie/WebM stickers are the largest weight items in this area.
2. **Video.** Is opening video in Quick Look enough, or must it play inside the message? Inline playback means linking a player into the shell.
3. **Mention autocomplete on Discord.** Completing a name that has not spoken recently needs a member search on the gateway as I type. The README promises tinyclient does not scrape member lists. Allow the per-keystroke search, or complete only from people already seen?
4. **Bot buttons.** Cutting interactive controls means I cannot approve a deploy or answer a bot prompt from tinyclient; I get an "Open in Slack" link. Acceptable?
5. **Disk.** Is a 1 GB default media cache right for a machine that is "already full", and should thumbnails download on any connection or only when a conversation is opened?
