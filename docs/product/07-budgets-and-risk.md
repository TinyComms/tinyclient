# 07. Budgets and account risk

A feature is done when it passes its budget, measured from outside the process. The 2026-10-13
baseline was not re-measured on 0.2.35: `chatd` 10 MB RSS signed in to Discord, shell 64 MB RSS
(77 MB earlier), no `mediad`. Those two figures are that day's baseline, not the current build.
The apps being replaced idle at about 2.4 GB. **Whole product, idle, every signed-in network:
150 MB.** That number is the gate, not a measurement of 0.2.35.

Items marked *(unverified)* come from community sources or are estimates not yet measured here.

## 1. Memory budgets

Measure with `ps -o rss= -p PID` (what e2e already uses) and `footprint -p PID` (`phys_footprint`
and its peak). `footprint` is the number that counts for the shell and mediad, because it includes
GPU textures and IOSurfaces that RSS misses. Read 60 s after the state is reached.

| Process and state | Budget | What it pays for |
| --- | --- | --- |
| chatd, idle, Discord + Slack + Teams | 20 MB RSS | 10 MB on 2026-10-13; 3 TLS sockets, SQLite's 2 MB page cache |
| chatd, idle, with Telegram | the chatd row above | grammers stays inside chatd. A comment in `telegram.rs` records about +0.2 MB footprint and +1.2 MB RSS over an empty process after the key exchange. Those figures are the comment's, not a 0.2.35 measurement. The old TDLib 30–50 MB row does not apply. |
| chatd, 100 Discord servers + 5 Slack workspaces, settled | +10 MB over idle | see the per-item costs below; everything else lives in SQLite |
| chatd, same account, peak while connecting | 64 MB `footprint` peak | READY is several MB of JSON *(unverified size)* and is held once as text while parsed |
| shell, list visible, no conversation | 80 MB footprint | 64 MB on 2026-10-13 is GPUI, Metal, fonts; 16 MB of room for avatars and the friends list |
| shell, conversation full of images | 125 MB footprint | 80 + 32 MB decoded image cap + 6 MB avatar cap + text |
| shell, after scrolling 5,000 messages | 125 MB, back to it within 10 s of stopping | the message window and both caches are capped, so history length costs nothing |
| shell, one image opened full size | +35 MB, released on close | one 2880 x 1800 px decode is 21 MB; the 4K original (33 MB) is never decoded whole |
| mediad, voice call | 40 MB footprint | Opus, jitter buffer, audio unit, encryption. Rules out linking all of libwebrtc *(estimate)* |
| mediad, watching or sending one share | 150 MB footprint | hardware codec; 6 pooled NV12 frames at 4K are 75 MB, at 1440p 33 MB |
| mediad --play, one voice message or audio file | 8 MB footprint, gone when paused or ended | 5.3 MB measured 2026-10-14 (CoreAudio speaker, AAC at 1.5x, under 1% CPU); 250 ms of audio ahead, one packet decoded at a time |
| mediad, no call | not running | already tested |
| chatd growth during a call | under 16 MB | already tested |

Per-item costs behind the heavy-account row (decoded pixels are 4 bytes each, at 2x):

- Channel, about 100 B: 100 servers x 50 channels = 0.5 MB. The ceiling of 500 channels per server
  is 5 MB, so channels of servers that are not open stay in SQLite, not in the adapter's map.
- User, about 120 B (id, two names, avatar hash): 1,000 friends, Discord's cap, = 120 KB.
- Presence, 32 B: friends, DM partners and one visible member window is about 1,200 entries =
  40 KB. A 100,000-member server would be 3 MB and a constant event stream; never subscribed.
- Custom emoji, 40 B: 100 servers x up to 250 = 1 MB, on disk only.
- Slack: about 0.3 MB per workspace socket; 200 joined channels each = 100 KB in total.
- Avatar, 40 pt = 80 x 80 px = 26 KB. About 40 are visible; cap 256 in memory = 6.5 MB.
- Inline image, at most 400 x 300 pt = 800 x 600 px = 1.9 MB. Three or four are visible. Keep the
  visible screen plus one screen each way, about 16 images: cap 32 MB.
- A GIF of that size with 100 frames would be 192 MB decoded whole, so it never is: frames are
  decoded at the size drawn, at most 120 of them and 16 MB a picture (2 MB for an emoji, avatar or
  picker cell), smaller first (to half) and then with fewer frames. All moving pictures in
  messages and pickers hold at most 48 MB of frames, avatars 8 MB; one that scrolls away keeps
  its first frame only. Measured 2026-10-14 (demo `#photo-dump`, 30 GIFs and animated WebPs,
  release build, 1120 x 720 at 2x): 8.1 MB of frames on screen, footprint 84 MB held with
  autoplay on against 76 MB with it off; after scrolling 4,000 pt through the wall, 11.5 MB of
  frames and 4.5 MB of first frames.

## 2. Feature threats and the rule that defuses each

| Feature | Rule | Reason |
| --- | --- | --- |
| Avatars | Request the CDN size that matches the pixels drawn (`?size=80` or `128`), store the file on disk, decode on a background thread, evict off-screen ones past the cap. Initials show until it arrives. | Full-size avatars are 1024 px = 4 MB each decoded. |
| Images | Ask the network for a thumbnail no larger than 800 px wide. Decode only inside the keep window. Layout uses the width and height from the message, so nothing reflows when the image lands. | Keeps scroll at 120 Hz and the cache at 32 MB. |
| GIFs and video | GIF, WebP and APNG: frames at the drawn size within the caps above, playing only while drawn, on screen and the window is key; one timer per window draws at the next frame due, never at the display's rate. Ask for the network's small moving copy (Discord's proxy `animated=true`, Tenor `tinygif`, Slack `thumb_480_gif`, Giphy `200.webp`), never the original. Autoplay is a setting (always, on hover, never); Reduce Motion plays only under the pointer. MP4 GIFs and video: the system's player until an inline player exists (`Pane::moving`). | 192 MB per decoded GIF; animation is the main idle-CPU leak in chat apps. |
| Presence | Subscribe to friends, DM partners, and members visible in the open channel. Drop the rest on arrival. | Bounded at about 1,200 entries regardless of server size. |
| Member lists | Request only the visible range of the open channel, in windows of 100, and drop it when the channel closes. Never page a whole server. | Memory, and paging members is what scrapers do (section 5). |
| Discord READY | Keep parsing into a struct that names only used fields. Add fields per feature; never keep the raw payload or a `Value` tree. Budget the peak. | A `Value` tree is 3 to 5 times the JSON size. |
| Slack boot | Keep `client.counts` as the only listing call. Page channel lists on demand when the browser is opened. No `client.userBoot`, no `users.list`. | Boot payloads are MBs on large workspaces *(unverified size)*. |
| Emoji | Unicode emoji come from the system font. Custom emoji are fetched when first drawn, at 48 px, into the avatar-style cache. The picker loads one server's set at a time. | A bundled emoji set is 10+ MB on disk and in memory. |
| Search | Search on Enter asks the network (`SearchNetwork`). Local search is `SearchMessages` over the cache. There is no FTS index. | An FTS index would add roughly 30 to 50% to the message table. It is not in this commit. |
| SQLite growth | Keep the newest 2,000 messages per conversation and nothing older than 90 days unless the conversation was opened in that time (`KEEP_MESSAGES`, `KEEP_DAYS`). Settings › Privacy can shorten the age. Prune at start and daily. | At about 300 B a row, 1,000 conversations x 2,000 = 600 MB without the age rule; with it, under 200 MB. |
| grammers | Runs on the adapter's thread inside chatd, and only while a Telegram account exists or a sign-in has started. It does not keep a second database. If a later measurement blows the chatd budget, that is a new decision. It is not the current design. | The TDLib 30–50 MB row does not apply. The +0.2 MB / +1.2 MB figures are the comment in `telegram.rs`. |
| Message window | The shell holds at most 300 messages; older ones page in 50 at a time from chatd and fall off the other end. | 5,000 shaped messages would otherwise cost tens of MB. |

## 3. Speed budgets

| What | Budget | How measured |
| --- | --- | --- |
| chatd launch to socket answering `ping` | 50 ms | e2e: time from spawn to first reply |
| Shell launch to cached list on screen and clickable | 400 ms | signpost at first frame, read with `xctrace`; cross-check with a 120 fps screen recording |
| `open` of a cached conversation, at the socket | 5 ms with 1 million stored messages | e2e against a seeded store |
| Click to conversation drawn | 50 ms, images arriving after without reflow | signpost click to frame |
| Incoming message, socket frame to `wait` waking | 10 ms | e2e (today it asserts under 1 s) |
| Incoming message, wake to pixels | 2 frames (17 ms) | signpost |
| Scroll through 5,000 messages with images | every frame under 8.3 ms, none over 16.7 ms | `MTL_HUD_ENABLED=1` frame-time graph; Instruments Animation Hitches |
| Key press to glyph | 1 frame at p95, 2 at p99 | signpost key event to frame |
| Sign-out | data gone and reply sent in 1 s | e2e |

No image decode, file read, socket call or SQLite query may run on the shell's main thread.

## 4. CPU and energy at idle

Measure with `top -l 5 -stats pid,command,cpu,idlew,power -pid PID` over 60 s with nothing arriving.

| Process | Budget | Today |
| --- | --- | --- |
| chatd, signed-in networks | 0.0% CPU, 1 wakeup/s or fewer | about 1.2 per adapter on 2026-10-13: `serve_commands` polls every 1 s, `Socket::read` every 5 s |
| shell, window visible, nothing changing | 0 frames drawn, 2 wakeups/s or fewer | blocks in `wait` for 30 s, which is right |
| shell, window hidden or occluded | 0 wakeups besides `wait` | |

- Block until there is work; wake only for the network's heartbeat (Discord about 41 s, Slack
  30 s). Reason: a 1 s poll in each adapter is a day's worth of wakeups for nothing.
- No polling request to any network. Sockets push. Reason: energy, and rate limits.
- Draw a frame only when state changed. The caret, typing dots and GIFs stop when the window is
  not key; relative times update on the minute. Reason: one animation holds the GPU awake.

## 5. Account safety

What is known:

- **Discord forbids user-account third-party clients.** Enforcement is by an anti-abuse system. The
  community API documentation says client properties (`X-Super-Properties`, with
  `client_build_number`) are "significant in anti-abuse systems" and are guessed from the
  User-Agent when absent. A flagged account gets a captcha (HTTP 400 with `captcha_sitekey`), forced
  phone verification (`USER_REQUIRED_ACTION_UPDATE`), a password reset, or is disabled.
- **Discord limits:** 50 requests/s per account; 10,000 responses of 401, 403 or 429 in 10 minutes
  earns a 24-hour IP ban; 120 gateway sends per 60 s; a send over 15 KiB closes the socket.
- **Long-lived clients imitate.** Abaddon sends a browser User-Agent, the web client's IDENTIFY, API
  v9, and only endpoints the web client uses. Its README names what trips the filter most: joining
  or leaving servers, frequent reconnects, starting new DMs, managing friends, editing the profile.
  Ripcord users reported more false positives from 2021 *(anecdotal)*. Dissent and discordo are
  believed to imitate too *(unverified)*.
- **Slack** documents four tiers (1, 20, 50, 100 calls per minute per method), one message per
  second per channel, and 429 with `Retry-After`. Enterprise audit logs raise anomaly events
  including `unexpected_client`, `unexpected_user_agent`, `session_fingerprint` (stale cookie),
  `unexpected_scraping`, `unexpected_api_call_volume` and `excessive_downloads`, and admins can
  reset a user's sessions. Whether an anomaly signs the user out is an org setting *(unverified)*.

## Identity (decided)

Discord and Slack use one browser identity for the whole session, from `protocol::browser`: installed Safari on macOS, installed WebView2's Chrome version on Windows, an estimated Chrome version on Linux. chatd does not switch to `tinyclient/x` after sign-in.

Discord is the web client on API v9: `X-Super-Properties`, gateway identify properties, `client_build_number` (live from the login page when it answers, otherwise a remembered build), and gateway resume. A dropped socket is not a fresh identify when the session can resume.

At connect, at most 5 unread direct conversations are fetched ahead, once per run, not again on resume.

The rules below still hold: nothing on a timer, no bulk friend or join automation, no whole-server member crawl, history for an opened conversation, one retry on 429, stop on a captcha. Risk stays in the README. The client shows state, not a warning banner.

A mix of identities is still the worst fingerprint. The session does not mix. Imitation is the choice this client ships, on purpose.

Rules:

1. One identity per session, identical in the sign-in helper, HTTP and the socket. Reason: a
   mid-session change is a named Slack anomaly and an inconsistency on Discord.
2. Every request traces to a user action or the socket; nothing runs on a schedule. Reason:
   machine cadence is what abuse detection looks for.
3. Never automate friend requests, new DMs, joining or leaving servers, invites, profile edits,
   reactions, bulk mark-read or bulk download. One click, one request, no retry loop. Reason:
   these are the endpoints that draw captchas and bans.
4. Never enumerate: no whole-server member requests, no `users.list`, no history crawl for cache
   or search. Reason: indistinguishable from scraping.
5. Fetch history only for an opened conversation. Warm at most 5 unread ones at connect, with
   jittered gaps, and not again on reconnect. Reason: official clients fetch what is on screen.
6. Resume the Discord gateway session instead of identifying again; back off 1 s to 60 s with
   jitter. Reason: frequent fresh sessions are a listed trigger.
7. On 429 wait the full `Retry-After`, retry once, then tell the user. On 401 or 403 stop. Stay
   under half of every documented limit. Reason: the invalid-request ban.
8. Send typing at most once per 8 s per conversation, only while the draft is changing.
9. On a captcha, phone or password demand: stop every request for that network, show "needs
   attention", and have the user finish in the official app. Never solve, proxy or retry a
   captcha. Reason: retrying a challenged action is how a flag becomes a ban.
10. No proxying, and save Slack's `d` cookie when the server reissues it. Reason: IP, ASN and
    stale-cookie anomalies.
11. Risk warnings stay in the README. The client shows states, never warnings.

## 6. Disk and privacy

| OS | Support directory |
| --- | --- |
| macOS | `~/Library/Application Support/tinyclient` |
| Windows | `%APPDATA%\tinyclient` |
| Linux | `$XDG_DATA_HOME/tinyclient` or `~/.local/share/tinyclient` |

`TINYCLIENT_DIR` replaces it for tests. `chat.sqlite` and `media/<network>/` (avatars, thumbnails, custom emoji) live there, files named by content hash. Sessions stay in the keychain or the system keyring, one item per network. Slack workspaces and Tiny servers share that one item and are split by the adapter. Nothing goes to temp directories; logs hold no message bodies or tokens.

- Caps: media 500 MB (`CACHE_BYTES`), least recently used evicted; messages default to 2,000 per conversation and 90 days (`KEEP_MESSAGES`, `KEEP_DAYS`), unless the conversation was opened in that window. Settings › Privacy can shorten the age. Under 1 GB in total, checked with `du -sk`.
- Full-size images and files are fetched when opened and saved only where the user chooses.
- Sign-out deletes that network's rows, its `media/` directory and its session, then vacuums so the bytes leave the file. e2e check: no row and no file for that network remains.
- The cache is unencrypted and relies on the disk's own encryption. The README already says so.

## Multiple accounts

Slack workspaces and Tiny servers are multiple accounts. They are not cut. A second Discord or Telegram login is not a separate keychain account in this commit.

## Telegram weight

grammers runs on the adapter's thread inside chatd, and only while a Telegram account exists or a sign-in has started. It does not keep a second database. The TDLib 30–50 MB row does not apply. If a later measurement blows the chatd budget, that is a new decision. It is not the current design.

Sources: Discord Userdoccers ([reference](https://docs.discord.food/reference), [rate limits](https://docs.discord.food/topics/rate-limits), [gateway](https://docs.discord.food/topics/gateway)), [Abaddon README](https://github.com/uowuo/abaddon), Slack ([rate limits](https://docs.slack.dev/apis/web-api/rate-limits), [anomaly events](https://docs.slack.dev/reference/audit-logs-api/anomalous-events-reference)).

## Historical overruns

The `settings` branch, before that work merged, measured a stripped shell at +590,032 B against a +400 KB budget and a stripped mediad at +118,592 B against +20 KB. Those rows are not a gate on main. The bytes are described as history in `09-settings.md`. Re-measure only if this file grows a new gate. The 2026-10-13 chatd 10 MB and shell 64 MB lines above are that day's baseline.
