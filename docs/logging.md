# Logging

Logs stay on this machine. Nothing is uploaded. `RUST_LOG` is ignored.

## Knobs

Environment, read at process start:

| Variable | Default | Values |
| --- | --- | --- |
| `TINYCLIENT_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
| `TINYCLIENT_LOG_RING` | `on` | `on` or `off` |
| `TINYCLIENT_LOG_EXPORT` | `on` | `on` or `off` |

The same three are settings `log_level`, `log_ring`, and `log_export`. A stored value replaces the environment after chatd opens its database. Garbage reads as the default. The subscriber admits our own crates up to `trace` from the start, and the render ceiling is what drops events, so raising the level takes effect for events after that change. `reqwest`, `hyper`, `h2`, `sqlx`, `tower`, and `str0m` stay at `warn` in the subscriber.

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
| `crash.log` | A panic, already passed through `redact` |

The old `chatd.log` at the top of the support directory is not opened, copied, or deleted.

Save debug log in Settings › About writes the window ring and asks chatd, which also asks a live mediad. Export off writes nothing and the window says "Debug logs are turned off." Bundles older than 24 hours are removed on the next save. On Unix the bundle is mode `0600`.

Stderr gets the same operator line when it is a terminal or `JOURNAL_STREAM` is set. chatd's stderr is otherwise discarded.

## What is kept out

Field names dropped on both sinks: `body`, `content`, `plaintext`, `text`, `caption`, `filename`, `file_name`, `media`, `ciphertext`, `password`, `authorization`, `cookie`, `token`, `secret`, `private_key`, `invite`, `oauth_code`, `code`, `verifier`, `nonce`, `refresh_token`, `id_token`, `access_token`, `key`, `ice_pwd`, `fingerprint`, `sdp`, `email`, `display_name`, `subject`, `phone`, `phone_number`, `vault`, `sig`, `detail`, and names ending in `_token`, `_secret`, `_password`, `_cookie`, or `_key`. `peer` is always dropped. `key_id`, `user_id`, `room_id`, `call_id`, and `session` stay.

Values go through `protocol::redact` (Slack and Discord tokens, JWTs, authorization headers, PEM, `a=ice-pwd`, `a=fingerprint`, URL userinfo) and are cut at 300 bytes. `reqwest`, `hyper`, `h2`, `sqlx`, `tower`, and `str0m` never log below `warn`.
