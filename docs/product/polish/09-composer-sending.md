# Polish 09 · The composer and everything you can send

Files, images, emoji, GIFs, stickers, voice, formatting and mentions, per network. Looked at:
`--seed-demo` shots of `#showcase` with `--open-first` and `TINYCHAT_SHOT_PICKER`; `composer.rs`,
`composer/*`, `input.rs`, `conversation/emoji.rs`; each adapter's `send`. Tags: **[C]** in our code,
**[W]** confirmed on the web (sources at the end), **[U]** unconfirmed, check on a live session.
This file reverses the cut of the GIF picker and of stickers in `stories.md` and `03`; voice
recording stays out of v1.

## 1. What is there today

| # | Finding |
|---|---|
| F1 | The card is a field and a send arrow, nothing else. No paperclip, no smile: nothing says files or emoji exist [C]. |
| F2 | Cmd-V of an image or of Finder files, and a drop, all end in one caption: "Sending files isn't built yet. Nothing was attached." The drop target is only the composer dock, not the conversation (`composer.rs:1127`) [C]. |
| F3 | `Send` is `text` + `reply_to` (`protocol/src/lib.rs:756`); each adapter's `send` takes `text` only [C]. |
| F4 | The emoji picker exists only for reactions: 199 hand-picked glyphs with no categories or skin tone. Recents live in a thread-local and are lost on quit ("CONTRACT: nothing stores them"). "This conversation" offers only the custom emoji already on its messages [C]. |
| F5 | Discord's READY guild `emojis` and stickers are not parsed. `premium_type` is not read, so the client cannot know what a Discord account may send [C]. |
| F6 | The `--composer <state>` shots need a `gui-shot` build. The debug binary drew the plain field for `refused`, so those states were not seen [C]. |

## 2. Rules

1. **Absent, not disabled.** A control shows only where this account, in this conversation, can
   use it. No locked icons and no upsell for Nitro or Premium. An emoji or sticker the account
   cannot send is not in the picker.
2. **The card holds at most four controls:** paperclip on the left; then the field; then GIF
   (Discord, Telegram), smile, and send. Stickers are a tab inside the one picker, not a button.
   Voice, when it exists, replaces send while the field is empty (Telegram only). Nothing else
   lives on the card. Options such as spoiler and send as file sit on the chips.
3. **One picker, three tabs:** Emoji · GIFs · Stickers. A tab with no source is absent. Reactions
   use the same picker with the Emoji tab only. It is built when it opens and dropped when it
   closes (as `emoji.rs` already is).
4. **Bytes never pass through the socket.** The shell stages paths. chatd streams each file from
   its path in parts and never holds the whole file. Pasted pixels become a file in
   `$TINYCHAT_DIR/outbox/`, deleted once sent or once the draft is dropped.
5. **Staging never uploads** (`04`). Send creates a pending row with its own progress (ruling 9).
   The composer is free at once, and Esc or the × on that row cancels the upload.
6. **Limits come from the session** (`03` rule 7). An oversize file is refused at staging, on its
   chip: "Too big for Discord (limit 20 MB)". No request is made.
7. **Originals by default.** No downscaling, except two conversions on a background thread at
   staging (ImageIO): Telegram's photo mode makes a JPEG of at most 2560 px and 10 MB (Telegram's
   photo rule; "Send as file" sends the original); HEIC becomes JPEG everywhere, with "Send
   original" on the chip [U: which networks show HEIC inline].
8. **One markup.** The field takes the Discord-style markdown people already type, with the
   marks left visible. Each adapter translates on send: Discord as typed, Slack `rich_text`
   blocks, Telegram entities, Teams HTML. A mark the network lacks (Slack spoiler, Telegram list)
   goes as literal text, never silently dropped.
9. **No URL is fetched to preview a link** (`03` rule 4). When the text holds a link, a quiet
   chip under the card reads "Preview: example.com ×". Clicking × suppresses the network's
   unfurl: Discord `SUPPRESS_EMBEDS`, Slack `unfurl_links/unfurl_media=false`, Telegram
   `no_webpage`. The chip is absent on Teams, where the sender builds the card and we send a bare link.
10. **Nothing loads before it is asked for.** No catalog, GIF or sticker is fetched before its
    tab opens. GIF search waits 300 ms and 2 characters, and a new query cancels the old one.
    Picker previews are still frames. Only the hovered GIF plays, and only one at a time, through
    AVFoundation. Animated stickers never play. Everything previewed lives on disk and is deleted
    when the picker closes.
11. **Server-side only.** Silent and scheduled sends (when they exist) are flags the network
    carries out. tinyclient never runs a timer that sends.

## 3. Per network

| | Discord | Slack | Telegram | Teams |
|---|---|---|---|---|
| Upload path (chatd, from the path) | `POST /channels/{id}/attachments` gives upload URLs; PUT each file; the message names the ids [C doc 03, U flow details] | `files.getUploadURLExternal` per file, POST the bytes, then one `files.completeUploadExternal` with `channel_id`, `thread_ts`, `initial_comment` = the text [W] | `upload.saveFilePart`, or `saveBigFilePart` over 10 MB, in 512 KB parts, which resume; then `sendMedia`, or `sendMultiMedia` for an album [W] | Image: AMS object with the skype token, then `<img>` in the HTML [U]. File: upload to my OneDrive "Microsoft Teams Chat Files", share it, `properties.files` [W folder, U API] |
| Size limit | Plan: 20 MB free (since Aug 2026, per Dexerto), 50 MB Basic, 500 MB Nitro [W]. Server boost raises it in that server [U]; take the larger | 1 GB per file, every plan [W] | 2 GB, 4 GB Premium, from config `upload_max_fileparts_*` [W]. Photo mode: 10 MB [U] | OneDrive's own (250 GB) [U]; inline image limit [U] |
| Several files | One message, ≤10 [U] | One message, many files [W] | Album of 2–10, one kind per album (photo/video, file, audio); 11+ become several albums [W] | One message [U] |
| Per-file options | Spoiler (`SPOILER_` filename); alt text P2 | none | Spoiler on photo/video; Send as file (no compression) | none |
| Caption | Text in the same message | `initial_comment` (mrkdwn, not blocks [U]) | ≤1024, Premium more, from config; longer text goes as its own message first | Text in the same message |
| Unicode emoji | yes | yes (adapter maps glyph to name) [C] | yes | yes |
| Custom emoji | Static, this server only, without Nitro. With Nitro: any server and animated. Sent as `<:n:id>` / `<a:n:id>` [U exact rules] | Workspace `emoji.list`, all sendable [C] | Premium only (`messageEntityCustomEmoji`), except a boosted group's own pack [W]. Hidden without Premium | Standard set; org custom emoji [U] |
| GIF source | Discord's `/gifs/search`, `/gifs/trending-gifs`, `/gifs/suggest` (it proxies Tenor/GIPHY) [W]. Send the result's `url` as text; Discord unfurls it as a `gifv`. Favorites in frecency settings [U] | Built-in picker exists (Slack says GIPHY), admin can turn it off [W]. Its client API is unknown [U]. **Hidden** until captured; never post a link we fetched from GIPHY ourselves | Saved GIFs `messages.getSavedGifs` / `saveGif`; search = inline query to `config.gif_search_username` [W], sent as the chosen result, no upload | Built-in GIPHY, a tenant toggle with a content rating (Moderate by default) [W]. Client API unknown [U]. **Hidden** until captured |
| Stickers | `sticker_ids` (≤3). This server's free, any with Nitro; standard packs Nitro only [W, third-party] | none | Installed packs, recent, favorites; static WEBP, TGS (Lottie), WEBM | none |
| Voice message | Flag `IS_VOICE_MESSAGE` + Ogg/Opus 48 kHz + waveform [W]. Official desktop cannot record; mobile only [W, support page; U for 2026] | Clips, ≤5 min [W] | Voice note (`documentAttributeAudio voice`, Ogg/Opus, waveform), on every official client | Mobile only [W] |
| Formatting sent | markdown | `rich_text` | entities | HTML |
| Niceties | Silent (`@silent` = flag 1<<12) [W]; reply ping toggle (`allowed_mentions.replied_user`) [W]; forward (`message_reference` type 1) [W] | Schedule (`chat.scheduleMessage`, ≤120 days) [W] | Silent (`silent`), schedule (`schedule_date`) [W]; send when online [U]; delete for everyone | Importance: Important / Urgent [U field] |
| Edit window | none | workspace setting [U pref name] | 48 h (`config.edit_time_limit`) [U value] | none, unless policy [U] |

## 4. The composer, shaped

- **Card** (left to right): paperclip 28 · field · `GIF` 28 · smile 28 · send 32. The tray of
  chips sits above the field (`04` frame 3). A chip carries a thumbnail or a type tile, the name
  (middle ellipsis), the size and ×. Its ⋯ menu holds Spoiler, Send as file and Send original,
  each only where it applies, and a set option shows as a small tag on the chip.
- **Keys:** Cmd-U file panel · Cmd-V image or Finder files · Cmd-E picker on Emoji · Cmd-Shift-G
  picker on GIFs · Tab inside the picker moves between tabs · `:` + 2 letters completes emoji ·
  Ctrl-Cmd-Space the macOS picker, through `input.rs`'s IME path · Option-Return sends silently
  (Discord, Telegram) · Cmd-B `**` · Cmd-I `*` · Cmd-Shift-X `~~` · Cmd-Shift-C `` ` `` (a
  multi-line selection becomes a fenced block; type the language after the fence). No formatting
  bar.
- **Drop:** the whole conversation pane is the target (`04` frame 9), not only the dock. One file
  or many stage as chips. A folder is refused on its chip: "Folders can't be sent. Zip it first."
- **Picker GIF tab:** a search field and a 3-column masonry of stills. Empty query shows
  Favorites or Saved, then Trending (Discord). Hover plays that cell. Click sends at once, with
  the composer's reply target. A star saves it to the network's favorites list (Discord frecency
  [U], Telegram `saveGif`). The footer names who sees the query: "Searches go to Discord" or "to
  Telegram's @gif".
- **Picker Stickers tab** (Telegram, then Discord): recent, favorites, then one row of pack
  icons. A pack's stickers load when its icon is clicked. Static thumbnails only.

## 5. Mentions and references

- People and channels complete as now. Add roles and user groups (Discord mentionable roles
  `<@&id>`, Slack `<!subteam^id>`; the adapter already reads `usergroups.list` [C]) ranked after
  people.
- `@here`, `@channel` and `@everyone` are offered only where the account may use them [U
  Discord permission bit]. If the message would notify more than 25 people, the first Return
  shows "This notifies 1,204 people in #general. Return again to send." in `warning`. Esc returns
  to editing.
- Someone not in the channel completes ranked last, with the note "not in #ops". After sending,
  the caption says they were not notified. No invite from tinyclient.
- `:` completion never opens inside a link or after a digit (10:30), and only one list is open at
  a time.

## 6. Stories

| # | I ... | Check | P |
|---|---|---|---|
| 1 | press Cmd-Ctrl-Shift-4 then Cmd-V, and a chip "Pasted image 9.41.12.png" appears; Return sends it with my text as the caption | The file is in `outbox/`; the official app shows one message with the image and text; `outbox/` is empty after | P0 |
| 2 | copy three files in Finder and paste, or drop them anywhere on the conversation | Three chips; nothing on the network until Return | P0 |
| 3 | send 5 images to Discord, Slack and Telegram | One message on Discord, one on Slack, one album on Telegram | P0 |
| 4 | attach a 30 MB video in a Discord DM on a free account | Refused on the chip with the limit; zero requests | P0 |
| 5 | send a 1.5 GB file to Telegram and keep typing | Pending row with byte progress; composer free; chatd RSS grows < 8 MB during upload | P0 |
| 6 | cancel mid-upload, or lose the network | Row offers Retry; Telegram resumes from the last part; text kept | P0 |
| 7 | try to send files while offline | Caption "Files need a connection"; chips and text stay; text alone still holds 60 s | P0 |
| 8 | open the smile button and pick from every Unicode emoji, with skin tone and recents that survive a restart | ~1,900 entries; recents persisted per account; picker RSS +<4 MB, 0 after close | P0 |
| 9 | type `:tada` and Return | 🎉 inserted; a Slack `:partyparrot:` and this Discord server's emoji are offered | P0 |
| 10 | search "cat" in GIFs on Discord and click one | One `/gifs/search` after a 300 ms pause; message is a Tenor/GIPHY link that unfurls; only hovered cell decodes | P0 |
| 11 | paste an image into a Teams chat | Inline image in Teams web [U AMS] | P1 |
| 12 | send my saved GIFs or an `@gif` result on Telegram | Message is an animation, no upload request | P1 |
| 13 | mark a Discord image spoiler, or send a Telegram photo as file | Blurred in official app; Telegram shows a document with original bytes | P1 |
| 14 | drop an iPhone HEIC | Chip says JPEG; Send original sends `.heic` | P1 |
| 15 | press Option-Return on Discord or Telegram | No notification on the second account's phone | P1 |
| 16 | send `@everyone` in a 500-member channel | Confirmation caption first; second Return sends | P1 |
| 17 | dismiss the link preview chip | Discord/Slack/Telegram show no card | P1 |
| 18 | toggle "ping" off when replying on Discord | Author gets no mention | P1 |
| 19 | send a Telegram sticker from my packs | Sticker message; no Lottie/WebM code linked | P1 |
| 20 | attach a file in a Teams chat | It lands in OneDrive "Microsoft Teams Chat Files" and opens for the other person | P1 |
| 21 | send a Discord sticker; mention a role or user group; schedule a Slack or Telegram message (server-side) | Each visible in the official app | P2 |
| 22 | hold to record a Telegram voice note | Recorded and encoded in `mediad` (it already has Opus), which exits after; chatd uploads the `.ogg` path | P2 |
| — | GIFs on Slack/Teams until their client API is captured; slash commands (a `/` message on Discord or Slack shows "Commands run in the official app; this posts as text"); `/remind`; Teams Urgent and Loop; Telegram polls, location, self-destruct; animated sticker playback; inline images in the field | | cut |

## 7. What the contract needs

- `Send` gains `attachments: [{path, name, spoiler, as_file}]`, `silent`, `suppress_embeds`,
  `ping_reply`, and one optional `sticker` or `gif` reference (an opaque id the adapter
  understands). `CancelSend {client_id}`.
- Change `Upload {client_id, sent, total}` at most 4 per second. The outcome comes through the
  existing message change. A failure names the file.
- Per-account (and per-conversation where it differs: Discord boosts, Telegram boosted packs)
  `Capabilities`: `max_file_bytes`, `max_files`, `caption_limit`, `spoiler`, `as_file`,
  `gif_source`, `stickers`, `custom_emoji_scope`, `everyone_allowed`, `edit_window`. They are sent
  with the account and cost bytes.
- `EmojiCatalog {account, space}`, `GifSearch {account, query, cursor}` / `GifSaved`,
  `StickerPacks` / `StickerPack {id}`. Answered on demand; previews written by chatd to
  `media/<net>/pick/` and deleted when the picker closes; custom emoji kept in the existing 48 px
  cache. Only Discord READY's emoji and sticker lists are kept between opens, at about 40 B a row
  (`07`).
- Recents and frequents in SQLite per account (glyph or id, count, last use; at most 36 kept).
- Budgets: picker closed = 0. Open: under 4 MB plus one playing MP4. Upload buffer: at most one
  part (512 KB on Telegram, 8 MB elsewhere). Account safety: every call follows a key press;
  existing route buckets are respected; Telegram's inline bot is called only while the GIF tab
  has a query.

## 8. Open questions for the owner

1. Scheduled send is server-side (rule 11), yet `stories.md` says "no scheduled sends". Allow it at P2?
2. Teams files need a second token audience (OneDrive or SharePoint) at sign-in, which a tenant
   may block. Ask for it on first attach (recommended), or never?
3. Voice notes at P2, Telegram first. Raise them to P1 for the Telegram-first persona?
4. Slack and Teams GIFs stay hidden until we capture each client's own API. Acceptable, or post
   GIPHY links through our own API key, which sends your queries to GIPHY?

Sources: [docs.discord.food GIFs](https://docs.discord.food/resources/integration),
[docs.discord.food messages](https://docs.discord.food/resources/message),
[Dexerto, Discord 20 MB](https://www.dexerto.com/entertainment/discord-raises-free-upload-limit-to-20mb-after-lowering-it-two-years-ago-3398251/),
[Discord voice messages](https://support.discord.com/hc/articles/13091096725527),
[Slack GIFs](https://slack.com/intl/en-gb/help/articles/26859384579219-Allow-GIFs-in-Slack),
[chat.scheduleMessage](https://docs.slack.dev/reference/methods/chat.scheduleMessage),
[Telegram files](https://core.telegram.org/api/files), [Telegram GIFs](https://core.telegram.org/api/gifs),
[Telegram scheduled](https://core.telegram.org/api/scheduled-messages),
[Teams GIPHY rating](https://tomtalks.blog/blocking-and-filtering-giphys-in-microsoft-teams-why-do-it-spoil-all-the-fun/),
[Teams voice](https://teams.handsontek.net/2020/04/19/send-voice-messages-microsoft-teams/).
