# Telegram stories and live location sharing (Wave 15C)

Proves the Wave 15C Telegram non-call features (#540 and #452):

- **Stories (#540)**: per-person unseen-story rings, explicit-only `stories.readStories` on open, story reactions (`stories.sendReaction`), private story replies (`inputReplyToStory`), live `updateStory` / `updateReadStories` handling, and forwarded story media cards with a "Story" label.
- **Live location sharing (#452)**: one-time location (`inputMediaGeoPoint`), live location start (`inputMediaGeoLive` with 15m/1h/8h/indefinite duration and direct-chat proximity radius), rate-limited coordinate updates (`messages.editMessage`), explicit stop (`stopped: true`), stop-all, and restart recovery.

Runs `chatd` with `TINYCLIENT_PROVE=telegram-wave15c` using invented fixture data and no Telegram login.
