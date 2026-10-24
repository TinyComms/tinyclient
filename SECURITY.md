# Security

tinyclient holds sessions for your chat accounts and a cache of your messages. A flaw in it can
expose both, so reports are taken seriously and handled privately.

## Reporting a vulnerability

Report it through GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/itz4blitz/tinyclient/security/advisories/new)
(the Security tab of this repository). Only the maintainers can see the report.

Do not open a public issue, pull request or discussion for a vulnerability. If private reporting
is unavailable for some reason, open an issue that says only that you have a security report and
asks for a private channel, with no details.

Please include:

- the version (`tinyclient` release or commit) and the operating system;
- what an attacker needs (local access as another user, a malicious message, a malicious network
  response, ...) and what they gain;
- steps or a proof of concept. Redact your own tokens: `protocol::redact` does this for logs.

You will get an acknowledgement, then a fix or an explanation. Once a fix is released the
advisory is published, crediting you unless you ask otherwise. tinyclient is a volunteer project
with no bug bounty.

Only the latest release and `main` receive security fixes.

## In scope

- **Credentials.** A session, token, cookie, refresh token, Telegram key or Signal key leaving the
  system credential store other than to its own network: in a log, `crash.log`, the database, a
  frame on the local socket, a child process's arguments, or a request to another host.
- **The local cache.** Another local user reading `chat.sqlite`, `media/` or `signal/`, or data
  that survives sign-out when it should not.
- **Untrusted input.** Anything a network or another user can send (a message, an attachment, an
  image, a link card, a websocket frame, a file name) that crashes a process, writes outside the
  data directory, runs code, or makes chatd fetch a host outside the network's allow-list.
- **The local socket and the mediad pipe.** A frame that crashes chatd or mediad, or escapes the
  1 MiB frame limit.
- **Voice.** Weaknesses in mediad's handling of RTP, transport encryption or DAVE (Discord's
  end-to-end encryption).
- **Signal.** Anything that weakens what Signal's protocol promises, in the adapter or the sealed
  state store (`crates/chatd/src/adapters/signal/vault.rs`).
- **The release pipeline.** `.github/workflows/` and `packaging/`.

## Out of scope

- The networks' own services and apps. Report those to Slack, Discord, Telegram, Microsoft or
  Signal.
- Account actions a network takes because a third-party client was used. That risk is described
  in the README under "Read this before you sign in".
- Attacks that need the ability to run code as your user, or to unlock your session: such an
  attacker can already read your files and ask your keychain. The limits below say what that
  means.
- Bugs in dependencies that tinyclient does not reach. Report reachable ones here and upstream.

## The security model

What tinyclient protects, and how. Every point is checkable in the code named.

**Processes.** Four programs, each with the least it needs (`docs/architecture.md`):

- `chatd` holds the sessions, the network connections, SQLite and the media cache. It never
  decodes media and never fetches a URL found in a message.
- `tinyclient`, the window, never talks to a network and never reads the database or the
  credential store. It decodes images for rows on screen.
- `mediad` exists only during a call. It speaks only to chatd over stdin and stdout and never
  touches SQLite, the credential store or the chat socket.
- `tinyclient-login` shows a network's sign-in page, prints the session on stdout to chatd, and
  exits.

**Sessions** are kept in the operating system's credential store (macOS Keychain, Windows
Credential Manager, Linux Secret Service) under the service `tinyclient`, one item per network
(`crates/chatd/src/session.rs`). A build whose credential store would not persist refuses to keep
sessions at all rather than fall back to memory or disk. On macOS the release is ad-hoc signed, so
each item's access list allows any app running as the same user to read it without a prompt.
That is what stops a login-password dialog after every update. The keychain stays locked when
the user is logged out. A Developer ID signature can restrict the list to chatd.

**Signal's state** (identity keys, prekeys, sessions, group keys, contacts) is in
`signal/<account>.state`, each value sealed with AES-256-GCM-SIV under a key held only in the
credential store item, with the value's name as associated data. The directory is created `0700`
and the file `0600` on Unix.

**The message cache** (`chat.sqlite`) and media cache (`media/`) are not encrypted. They rely on
disk encryption and the permissions of your home directory.

**Network connections** go only to the networks you are signed in to. Media is fetched only from
hosts on a per-network allow-list (`crates/chatd/src/blobs/fetch.rs`), uploads go only to hosts on
another (`crates/chatd/src/upload.rs`), and only over HTTPS. Test overrides of any address
(`TINYCLIENT_*_API` and the like) are honoured only when `TINYCLIENT_DIR` is set, which also
turns off the credential store, so a stray variable cannot send a real session elsewhere
(`crates/chatd/src/adapters/net.rs`).

**TLS** is the system's (Security.framework, SChannel, OpenSSL through `native-tls`), except for
Signal, whose libraries use rustls pinned to Signal's own certificate authority.

**Logs.** `chatd.log` and `crash.log` stay in the data directory. Panic reports and connection
errors pass through `protocol::redact`, which removes Slack, Discord and Teams tokens, JWTs,
authorization headers, signed URL parameters and Signal's passwords and keys
(`crates/protocol/src/crash.rs`). There is no telemetry and no crash upload: "Copy report" in the
window copies the last entry to your clipboard and nothing more.

**Frames** on the socket and between chatd and mediad are length-prefixed JSON, at most 1 MiB
either way; larger frames are refused before they are read or written.

**Sign-out** asks the network to end the session where it can, deletes the credential store item
(or the one account from it), the account's rows in the database and its media directory. For
Signal it also unlinks the device.

## Known limits

Stated plainly so nobody relies on what is not there:

- **The local socket is not authenticated.** Any process running as your user can connect to
  `chat.sock` (a named pipe on Windows) and read or send messages. It is protected only by the
  permissions of the data directory and the operating system's defaults for sockets and pipes.
- **The cache is not encrypted at rest**, and sign-out deletes rows without vacuuming the
  database, so deleted content can remain in free pages of `chat.sqlite` until SQLite reuses them.
- **The sign-in window's browser data** (cookies of the sign-in page) is kept so an interrupted
  sign-in resumes, and is not cleared on sign-out: a WebKit data store on macOS, `login/<network>/`
  in the data directory elsewhere.
- **Signal safety numbers are not shown**, and a contact's new identity key is trusted
  automatically, as presage's own stores do. A key change is not surfaced to you yet.
- **macOS releases are signed ad hoc**, not with a Developer ID, and not notarized. Check that a
  download came from this repository's Releases page.
- **The window decodes images** sent by other people (PNG, JPEG, GIF, WebP, BMP through the
  `image` crate) in the GUI process.
