# tinyclient

The process map is `docs/architecture.md`. These are the invariants a review holds the diff to.

## Who may do what

- **`chatd`** holds sessions, sockets, SQLite and the media cache. It never decodes media, links a
  codec or audio code, holds a tree, a member list or history in memory, fetches a link from a
  message, or sends anything the user did not ask for.
- **`mediad`** owns one call: Discord's voice gateway, UDP, Opus and DAVE, or a Tiny call's WebRTC
  (str0m) with Opus and H.264. It never touches SQLite, the keychain or the chat socket.
- **`tinyclient`** is the window. It never reads the database or keychain, runs a socket call, or
  does a file read or a decode on the main thread. It links no Opus or H.264.
- **`tinyclient-login`** shows one sign-in window and exits with the session on stdout.
- Code that crosses these lines (a new dependency from the window into chatd's internals, a
  codec linked into chatd, SQLite reached from mediad) is a blocker.

## The wire between them (`crates/protocol`)

- Frames are a 4-byte length then JSON, at most 1 MiB. A change that reads or builds a frame
  without that cap is a blocker.
- Every field added after the first cut has `#[serde(default)]` and is left off the wire when
  empty, so an older frame still reads. Removing, renaming or repurposing a field, or adding an
  enum variant an older peer would fail on, is a blocker.
- Everything a network could make large is capped: `PAGE_LIMIT` 200 rows, a character cap on
  every string that is not a message's text, `Message::bound` on the way into the store. A new
  list or string from a network with no cap is a major, a blocker if it reaches the window.
- An account id is one plain file name (letters, digits, `.`, `_`, `-`; never `.` or `..`)
  because it names a directory. Building a path from any other network string is a blocker.
- The keychain holds one item per network, service `tinyclient`. Secrets go there, never into a
  file, SQLite, an env var, a log or the protocol. `protocol::redact` is how a token leaves a log.

## Encryption (`crates/e2ee`, the Tiny adapter, `mediad` calls)

- Messages in an encrypted conversation are never sent plain, and there is no downgrade. A code
  path that falls back to plaintext on an error is a blocker.
- A device is added only when it chains to the person's first device by a cross-signature. A
  commit or proposal not signed by the pinned server key is invalid. An epoch that goes backwards
  freezes the group. A change that weakens one of these checks, or skips it on an error path, is a
  blocker.
- Keys, nonces, MLS secrets and sealed-file keys are never logged, formatted into an error,
  compared with `==`, or kept in a long-lived struct that derives `Debug` or `Clone` without need.
  Prefer zeroizing types the crate already uses.

## Calls and media

- The audio and video paths run on a real-time cadence. No allocation, lock, log line or syscall
  that can block in a per-frame callback. Frame sizes and counts from the network are checked
  before they size a buffer.
- Echo cancellation, noise suppression and automatic gain default on. A change that flips a
  default needs to say so.

## Networks

- Each adapter talks to its network directly. Nothing the user types goes to a Tiny-owned server
  unless they added that server. A new outbound host (analytics, a crash uploader, a link
  unfurler, a font or image CDN) is a blocker unless the PR says why and the user can see and
  turn it off. The update check (GitHub) and a linked article's summary (Wikipedia) are the two
  that exist.
- Rate limits and reconnects: a retry loop with no backoff, or one that retries a 401/403, is a
  major. A network's undocumented API used with a hard-coded secret is a blocker.

## Mobile

`mobile/` builds Android and iOS from the same crates. Platform code must not assume a desktop:
no spawning `chatd` as a separate process where the platform forbids it, no desktop paths.
