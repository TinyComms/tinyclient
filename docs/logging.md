# Logging

Logs stay on this machine. Nothing is uploaded. `RUST_LOG` is ignored.

## Knobs

Environment, read at process start:

| Variable | Default | Values |
| --- | --- | --- |
| `TINYCLIENT_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
| `TINYCLIENT_LOG_RING` | `on` | `on` or `off` |
| `TINYCLIENT_LOG_EXPORT` | `on` | `on` or `off` |

The same three are settings `log_level`, `log_ring`, and `log_export`. A stored row replaces that one environment value after chatd opens its database, and again in the window when settings load. A missing row leaves the environment value. Garbage reads as the default. The subscriber admits our own crates up to `trace` from the start, and the render ceiling is what drops events, so raising the level takes effect for events after that change. `reqwest`, `hyper`, `h2`, `sqlx`, `tower`, and `str0m` stay at `warn` in the subscriber.

Verbose logging (Settings › About, and the Logs window) stores `log_level` as `debug`, and clears it when turned off (or stores `info` when `TINYCLIENT_LOG_LEVEL` alone would keep debug). chatd moves its own ceiling when the row is stored, and sends a running call's or microphone test's mediad `MediaRequest::LogLevel`; the window moves when its settings load again. A mediad or login helper started later gets the ceiling in `TINYCLIENT_LOG_LEVEL` beside `TINYCLIENT_SESSION`. Nothing needs a restart, and the row is kept for the next start.

`TINYCLIENT_SESSION` is 32 hex characters shared by the window, chatd, and mediad. The window adopts chatd's id from `Status.session` when that field is set.

## Files

Under the support directory (`TINYCLIENT_DIR`, otherwise the platform data dir):

| File | What it is |
| --- | --- |
| `logs/tinyclient.log` | The window. Rolls at 2 MiB to `.1` and `.2` |
| `logs/chatd.log` | The background service. Same roll |
| `logs/mediad.log` | Voice. Same roll |
| `logs/login.log` | The sign-in window. Same roll |
| `logs/bundles/<session>-<process>-<utc>.log` | The in-memory ring, written only by Save debug log |
| `crash.log` | A panic, already passed through `redact`. Mode `0600` on Unix |

Each operator line is flushed as it is written. The window also records lifecycle lines (`logging to`, `window open`, `window should close`, `close window`, `quit asked`, `window gone`) so a hang or a vanished HWND still leaves a reason on disk.

The old `chatd.log` at the top of the support directory is not opened, copied, or deleted.

Save debug log in Settings › About writes the window ring and asks chatd, which also asks a live mediad. Export off writes nothing and the window says "Debug logs are turned off." Bundles older than 24 hours are removed on the next save. On Unix the log directory is mode `0700`, and the operator files and the bundle are mode `0600`.

The Logs window (Help › Logs, ⇧⌘L / Ctrl+Shift+L, or View logs in Settings › About) reads `tinyclient.log`, `chatd.log`, and `mediad.log` while it is open, every half second, and keeps the last 5000 lines of each in memory. It filters by process, level, and words, and its Hide noise switch (on by default) drops GPUI's "RefCell already borrowed" and "No monitor detected!" lines. Copy visible puts the lines it shows on the clipboard. It shows only what the files already hold, so nothing it shows has skipped the redaction below.

Stderr gets the same operator line when it is a terminal or `JOURNAL_STREAM` is set. chatd's stderr is otherwise discarded.

## What is kept out

Field names dropped on both sinks: `body`, `content`, `plaintext`, `text`, `caption`, `filename`, `file_name`, `media`, `ciphertext`, `password`, `authorization`, `cookie`, `token`, `secret`, `private_key`, `invite`, `oauth_code`, `code`, `verifier`, `nonce`, `refresh_token`, `id_token`, `access_token`, `key`, `ice_pwd`, `fingerprint`, `sdp`, `email`, `display_name`, `subject`, `phone`, `phone_number`, `vault`, `sig`, `detail`, and names ending in `_token`, `_secret`, `_password`, `_cookie`, or `_key`. `peer` is always dropped. `key_id`, `user_id`, `room_id`, `call_id`, and `session` stay.

Values go through `protocol::redact` (Slack and Discord tokens, JWTs, authorization headers, PEM, `a=ice-pwd`, `a=fingerprint`, URL userinfo) and are cut at 300 bytes. `reqwest`, `hyper`, `h2`, `sqlx`, `tower`, and `str0m` never log below `warn`.
