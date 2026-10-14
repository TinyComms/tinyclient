# 07. Budgets and account risk

A feature is done when it passes its budget, measured from outside the process. Baseline on
2026-10-13: `chatd` 10 MB RSS signed in to Discord, shell 64 MB RSS (77 MB earlier), no `mediad`.
The apps being replaced idle at about 2.4 GB. **Whole product, idle, four networks: 150 MB.**

Items marked *(unverified)* come from community sources or are estimates not yet measured here.

## 1. Memory budgets

Measure with `ps -o rss= -p PID` (what e2e already uses) and `footprint -p PID` (`phys_footprint`
and its peak). `footprint` is the number that counts for the shell and mediad, because it includes
GPU textures and IOSurfaces that RSS misses. Read 60 s after the state is reached.

| Process and state | Budget | What it pays for |
| --- | --- | --- |
| chatd, idle, Discord + Slack + Teams | 20 MB RSS | 10 MB today; 3 TLS sockets, SQLite's 2 MB page cache |
| chatd, idle, four networks | 70 MB RSS | the above plus TDLib, 30 to 50 MB *(unverified, must be measured before Telegram is designed in)* |
| chatd, 100 Discord servers + 5 Slack workspaces, settled | +10 MB over idle | see the per-item costs below; everything else lives in SQLite |
| chatd, same account, peak while connecting | 64 MB `footprint` peak | READY is several MB of JSON *(unverified size)* and is held once as text while parsed |
| shell, list visible, no conversation | 80 MB footprint | 64 MB today is GPUI, Metal, fonts; 16 MB of room for avatars and the friends list |
| shell, conversation full of images | 125 MB footprint | 80 + 32 MB decoded image cap + 6 MB avatar cap + text |
| shell, after scrolling 5,000 messages | 125 MB, back to it within 10 s of stopping | the message window and both caches are capped, so history length costs nothing |
| shell, one image opened full size | +35 MB, released on close | one 2880 x 1800 px decode is 21 MB; the 4K original (33 MB) is never decoded whole |
| mediad, voice call | 40 MB footprint | Opus, jitter buffer, audio unit, encryption. Rules out linking all of libwebrtc *(estimate)* |
| mediad, watching or sending one share | 150 MB footprint | hardware codec; 6 pooled NV12 frames at 4K are 75 MB, at 1440p 33 MB |
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
- A GIF of that size with 100 frames is 192 MB decoded. It is never decoded as frames.

## 2. Feature threats and the rule that defuses each

| Feature | Rule | Reason |
| --- | --- | --- |
| Avatars | Request the CDN size that matches the pixels drawn (`?size=80` or `128`), store the file on disk, decode on a background thread, evict off-screen ones past the cap. Initials show until it arrives. | Full-size avatars are 1024 px = 4 MB each decoded. |
| Images | Ask the network for a thumbnail no larger than 800 px wide. Decode only inside the keep window. Layout uses the width and height from the message, so nothing reflows when the image lands. | Keeps scroll at 120 Hz and the cache at 32 MB. |
| GIFs and video | Show the first frame. Play one at a time, only while visible and the window is key, through AVFoundation's hardware decoder (Discord, Tenor and Giphy serve MP4). | 192 MB per decoded GIF; animation is the main idle-CPU leak in chat apps. |
| Presence | Subscribe to friends, DM partners, and members visible in the open channel. Drop the rest on arrival. | Bounded at about 1,200 entries regardless of server size. |
| Member lists | Request only the visible range of the open channel, in windows of 100, and drop it when the channel closes. Never page a whole server. | Memory, and paging members is what scrapers do (section 5). |
| Discord READY | Keep parsing into a struct that names only used fields. Add fields per feature; never keep the raw payload or a `Value` tree. Budget the peak. | A `Value` tree is 3 to 5 times the JSON size. |
| Slack boot | Keep `client.counts` as the only listing call. Page channel lists on demand when the browser is opened. No `client.userBoot`, no `users.list`. | Boot payloads are MBs on large workspaces *(unverified size)*. |
| Emoji | Unicode emoji come from the system font. Custom emoji are fetched when first drawn, at 48 px, into the avatar-style cache. The picker loads one server's set at a time. | A bundled emoji set is 10+ MB on disk and in memory. |
| Search | Search asks the network. Local search is SQLite FTS5 over cached messages only if the owner wants it (question 3). | An FTS index adds roughly 30 to 50% to the message table. |
| SQLite growth | Keep the newest 2,000 messages per conversation and nothing older than 90 days unless the conversation was opened in that time. Prune at start and daily. | At about 300 B a row, 1,000 conversations x 2,000 = 600 MB without the age rule; with it, under 200 MB. |
| TDLib | Give it its own row above and measure first. If it exceeds 50 MB, run it as its own process that starts only when Telegram is signed in. | It is the single largest cost in chatd and has its own database. |
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
| chatd, four networks | 0.0% CPU, 1 wakeup/s or fewer | about 1.2 per adapter: `serve_commands` polls every 1 s, `Socket::read` every 5 s |
| shell, window visible, nothing changing | 0 frames drawn, 2 wakeups/s or fewer | blocks in `wait` for 30 s, which is right |
| shell, window hidden or occluded | 0 wakeups besides `wait` | |

- Block until there is work; wake only for the network's heartbeat (Discord about 41 s, Slack
  30 s). Reason: a 1 s poll in four adapters is 345,000 wakeups a day for nothing.
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
- **tinyclient on Discord today is honest and anomalous:** User-Agent `tinyclient/x`, IDENTIFY browser
  `tinyclient`, API v10, no super properties, no gateway RESUME (every drop is a fresh IDENTIFY), and
  up to 20 history fetches at a fixed 600 ms after each connect.
- **Slack** documents four tiers (1, 20, 50, 100 calls per minute per method), one message per
  second per channel, and 429 with `Retry-After`. Enterprise audit logs raise anomaly events
  including `unexpected_client`, `unexpected_user_agent`, `session_fingerprint` (stale cookie),
  `unexpected_scraping`, `unexpected_api_call_volume` and `excessive_downloads`, and admins can
  reset a user's sessions. Whether an anomaly signs the user out is an org setting *(unverified)*.
- **tinyclient on Slack today changes User-Agent mid-session:** the sign-in helper presents Safari,
  then chatd uses the same token as `tinyclient/x`. That is what `unexpected_user_agent` describes.

Honest identification (`tinyclient` everywhere) deceives nobody and matches the README, but gives
every user one fingerprint a single server rule can flag, without the fields Discord's system
weighs. Imitative identification (the web client's User-Agent, properties, build number, API
version, endpoint set) is what surviving Discord clients do; it is deliberate impersonation, a
stale build number becomes a signal itself, and on a work Slack it hides the client from the
employer's security team. A mix is worst: an inconsistent fingerprint is more suspicious than either.

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

- Everything is under `~/Library/Application Support/tinyclient/`: `chat.sqlite` and
  `media/<network>/` (avatars, thumbnails, custom emoji) as files named by content hash. Sessions
  stay in the keychain. Nothing goes to temp directories; logs hold no message bodies or tokens.
- Caps: media 500 MB, least recently used evicted; database by the retention rule in section 2.
  Under 1 GB in total, checked with `du -sk`.
- Full-size images and files are fetched when opened and saved only where the user chooses.
- Sign-out deletes that network's rows (true today), its `media/` directory and its session, then
  vacuums so the bytes leave the file. e2e check: no row and no file for that network remains.
- The cache is unencrypted and relies on FileVault. The README already says so.

## Open questions for the owner

1. **Discord identity: honest or imitative?** Imitation is safer for the account; honesty is easier
   to stand behind in a public release. Decide before avatars and presence ship, and never mix.
2. **Slack identity.** Should chatd present the helper's Safari string (consistent, but hidden from
   a work admin), or should both say `tinyclient`? Google and Okta sign-in may refuse the second.
3. **Local search.** Is the network's own search enough, or is an offline index worth the disk?
4. **Five Slack workspaces** conflicts with "multiple accounts per network" being cut in
   `stories.md`. Which stands?
5. **TDLib.** If it measures above 50 MB, is a separate Telegram process acceptable?

Sources: Discord Userdoccers ([reference](https://docs.discord.food/reference), [rate limits](https://docs.discord.food/topics/rate-limits), [gateway](https://docs.discord.food/topics/gateway)), [Abaddon README](https://github.com/uowuo/abaddon), Slack ([rate limits](https://docs.slack.dev/apis/web-api/rate-limits), [anomaly events](https://docs.slack.dev/reference/audit-logs-api/anomalous-events-reference)).
