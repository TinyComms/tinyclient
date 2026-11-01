# Security

tinyclient holds sessions for your chat accounts and a cache of your messages. A flaw in it can
expose both, so reports are taken seriously and handled privately.

## Reporting a vulnerability

Report it through GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/TinyComms/tinyclient/security/advisories/new)
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
  end-to-end encryption), and in a Tiny server call's end-to-end encryption: anything that lets
  the server, or anyone who is not in the call, hear it while the call says "End-to-end
  encrypted", or that makes the lock claim more than holds.
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
disk encryption and on who can read the folder. In plain text they hold the cached messages
(text, sender names, attachment names), your drafts, what search needs, and every image and file
you opened. Signing out deletes an account's rows and media directory; it does not vacuum the
database (see the limits below).

**Who can read the files on disk.** On Unix the support directory, each account's media folder, the call key folders and
the Signal state folder are mode `0700`, and `chat.sqlite` (with its `-wal` and
`-shm`), downloaded media files, `crash.log`, `e2ee/<account>/identity.key`, `verified.json` and
the Signal state file are `0600`; `chat.sock` is set to `0600` after it is bound. `chat.sqlite` is
created `0600` before SQLite opens it. What an older build made wider is narrowed when chatd next
starts (the support directory, the database and its logs), or when it is next written
(`crash.log`, `verified.json`, the identity key, an account's media folder); media files already
downloaded keep the mode they have, behind their folder (`crates/protocol/src/private.rs`). `verified.json` and the
identity key are written to a file beside them, flushed and renamed over, so a crash leaves the
old file or the whole new one. On Windows none of this changes a mode: the files inherit the
access list of `%APPDATA%\tinyclient` in your profile, and tinyclient does not alter it. What
stays outside this: `outbox/` (pictures pasted into the composer, written by the window), the
window's sign-in browser data, the `logs/` files (their own modes are in `docs/logging.md`) and
the lock file `chatd.lock`.

**Network connections** go only to the networks you are signed in to. Media is fetched only from
hosts on a per-network allow-list (`crates/chatd/src/blobs/fetch.rs`), uploads go only to hosts on
another (`crates/chatd/src/upload.rs`), and only over HTTPS. Test overrides of any address
(`TINYCLIENT_*_API` and the like) are honoured only when `TINYCLIENT_DIR` is set, which also
turns off the credential store, so a stray variable cannot send a real session elsewhere
(`crates/chatd/src/adapters/net.rs`).

**Tiny server text and files are not end-to-end encrypted.** A message's text, its attachments'
bytes and names go to the server as its API takes them
(`crates/chatd/src/adapters/tiny/messages.rs`, `upload.rs`) and the server stores them readable.
Only a Tiny server's calls are end to end (below). Encrypted text is being designed in
tinyserver (`docs/e2ee-text.md` there); none of it is in this client yet.

**Plain http to a Tiny server.** A Tiny server's address may be `http`, so a server on your own
machine or network works. The session's token and everything else sent over http can be read by
anyone on the way. So the sign-in sheet says so in words under any `http` address that is not this
computer, and what it does depends on where the address reaches (`protocol::reach`, from the
address text alone):

- this computer (`localhost`, `127.0.0.0/8`, `::1`): no warning;
- your own network (private and link-local addresses, Tailscale's `100.64.0.0/10`, a bare machine
  name, `.local`, `.lan`, `.home.arpa`, `.internal`, `.ts.net`): a warning, and it goes ahead;
- anything else: chatd sends nothing, not even a request for the server's sign-in page, until the
  sheet has said so and you continue again with the same address. That yes is saved in the session
  (`plain_http_ok`). A saved session for such a server without it, including one saved before this
  question existed, is not started and its commands are refused; it is still listed and you can
  sign out of it (`crates/chatd/src/adapters/tiny.rs`, `signin.rs`).

This is judged from the address, not from where a name resolves: a name that looks local but
resolves to a public address, or a network that is yours in name only, is not caught, and on a
"network of your own" anyone else on that network can still read the traffic.

**Updates.** The updater (`crates/tinyclient/src/update.rs`) runs curl with `--proto =https
--proto-redir =https` and follows no redirect on its own: the first address and each redirect (five
at most) must be `https` on `api.github.com`, `github.com`, `objects.githubusercontent.com` or
`release-assets.githubusercontent.com`. A release whose archive or `SHA256SUMS` asset points
elsewhere is not taken. The archive is checked against `SHA256SUMS` from the same release, and on
macOS the unpacked app must pass `codesign --verify --deep --strict`. What this does not give:
`SHA256SUMS` is published next to the archive, so it catches a damaged or swapped download but not
a release that was published maliciously, and the macOS check only shows that the app's ad-hoc
signature is intact, which anyone can produce. No signing key exists in this repository, so no
signature on the release is checked. Anyone who can publish a release to the TinyComms/tinyclient
repository, or who controls GitHub's answer, can ship code to a build that has automatic install
on. The repository is `TinyComms/tinyclient` (the old `itz4blitz/tinyclient` address only
redirects to it).

**Notifications** name the sender and say "New message" or "New mention", with no text, by
default (Settings › Notifications › "Show in notifications", `preview = sender`; the locked-screen
setting was already `sender`). The text appears in a notification only when you pick "Sender and
message". Settings are stored only when they differ from the default, so someone who had picked
"Sender and message" under the old default has no stored value and now gets "Sender only", and can
pick it again; a value anyone stored is read back as stored. What the system then does with a
notification, such as keeping a history of it, is the system's and outside tinyclient.

**TLS** is the system's (Security.framework, SChannel, OpenSSL through `native-tls`), except for
Signal, whose libraries use rustls pinned to Signal's own certificate authority.

**Logs.** Each process writes `logs/tinyclient.log`, `logs/chatd.log`, `logs/mediad.log`, or
`logs/login.log` under the support directory, rolling at 2 MiB with two older generations.
`logs/bundles/` is written only when you choose Save debug log, and nothing uploads those files.
Panic text still goes to `crash.log`. Every line passes through `protocol::redact`, which removes
Slack, Discord and Teams tokens, JWTs, authorization headers, signed URL parameters, PEM blocks,
ICE passwords, and Signal's passwords and keys (`crates/protocol/src/crash.rs`). There is no
telemetry and no crash upload. Copy diagnostics copies a short status blurb, not log lines.
"Copy report" copies the last panic entry to your clipboard and nothing more.

**Tiny server calls** are end-to-end encrypted when the server lists `e2ee_calls` and everyone in
the room's call has an app that can do it (`crates/mediad/src/e2ee.rs`). The apps in the call form
an MLS group (RFC 9420, ciphersuite `MLS_128_DHKEMP256_AES128GCM_SHA256_P256`, through `davey`,
the library behind Discord's DAVE); the server is only the group's external sender and delivery
service: it proposes adding whoever joins and removing whoever leaves and orders the commits,
but holds no key that opens a frame. Each Opus frame is sealed with the sender's key for the
current epoch (AES-128-GCM, 8-byte tag), so the SFU forwards ciphertext; the RTP header and the
RFC 6464 audio level stay in clear for it to route and to show who speaks. Camera and screen
share pictures (H.264) are sealed with the same group's keys, leaving only each NAL unit's one-byte
header in clear so the SFU can tell a keyframe; a picture that arrives in clear during an
end-to-end call is dropped, not shown. Every join and leave
is a new epoch with new keys. Each account has an identity key (P-256), made on its first call and
kept in `e2ee/<account>/identity.key` in the data directory (`0600` in a `0700` folder on Unix); it
signs the member's MLS credential, which names its user id on the server. The key is written to a
file beside it and renamed over, so a crash leaves the old key or the whole new one. A key file
that exists but cannot be read is not overwritten: it is moved to `identity.key.unreadable`, a
new key is made, and everyone who verified you sees your key change, which is correct because it
is a different key. Until the group has keys the microphone and camera send nothing, and a frame
that arrives in clear during an end-to-end call is dropped.

**The server's `off` is never obeyed silently.** `off` and `start` are unauthenticated JSON from
the server: nobody in the call signs them, so a server that wants to hear a call need only say
someone cannot encrypt. When it says `off`, mediad stops sealing, sending, opening and playing,
audio and video, and the call's badge reads "Encryption was turned off" with a warning icon,
never a lock, naming whom the server says cannot encrypt. Nothing goes out unencrypted until you
press "Carry on without encryption" in the call's security panel; that choice holds for that
seat only, until the server starts a group again or you join again, and then the next `off`
asks again. A later `start` from the server (for
instance once the person whose app could not do it has left) puts the call back in a group.

Some calls never carry on unencrypted, and the panel offers no button, only leaving:

- with **Only join end-to-end encrypted calls** on (Settings › Privacy › Calls,
  `require_call_e2ee`, off by default because an older tinyserver or one older app in a room
  cannot do it);
- on an **Incognito host** (`incognito.tinyclient.io` and names under it),
  whose communities promise end-to-end encrypted calls, whatever the setting;
- in a room the server lists as **`e2ee_required`**.

For those, chatd also does not join a server that does not list `e2ee_calls`, and says so;
nothing is sent to it for that call (`Calls::tiny_offer`). The setting and the room are read
when you join; changing either during a call applies to the next one. A call that never had
end-to-end encryption because the server lacks it (and is not one of those) says "Encrypted to
the server" from the start: that is not a downgrade.

It is a refusal to carry on, not a verification: the roster it protects is still the server's
word (see "Unverified people can be impersonated" below), and it covers Tiny calls only.

What you can check: everyone in the call sees the same 30-digit **call code**, derived from the
group's epoch authenticator; reading it aloud to each other proves you are all in one group,
and not in two groups the server relays between. Each other member has a 45-digit **safety number** for your key and theirs;
"Mark as verified" remembers their key (`e2ee/<account>/verified.json`), and a later different
key for them is shown as changed, loudly. Someone in the group the server does not list in the
call, or one person holding two places in it, is flagged too: the call code cannot catch an extra
member, who holds the epoch's keys like everyone else.

**Frames** on the socket and between chatd and mediad are length-prefixed JSON, at most 1 MiB
either way; larger frames are refused before they are read or written.

**Sign-out** asks the network to end the session where it can, deletes the credential store item
(or the one account from it), the account's rows in the database and its media directory. For
Signal it also unlinks the device.

## Known limits

Stated plainly so nobody relies on what is not there:

- **The local socket is not authenticated.** Any process running as your user can connect to
  `chat.sock` (a named pipe on Windows) and read or send messages. It is protected only by the
  permissions of the data directory (`0700`) and of the socket itself (`0600`, set after it is
  bound) on Unix, and the operating system's defaults for a named pipe on Windows.
- **The cache is not encrypted at rest**, and sign-out deletes rows without vacuuming the
  database, so deleted content can remain in free pages of `chat.sqlite` until SQLite reuses them.
- **The sign-in window's browser data** (cookies of the sign-in page) is kept so an interrupted
  sign-in resumes, and is not cleared on sign-out: a WebKit data store on macOS, `login/<network>/`
  in the data directory elsewhere.
- **Signal safety numbers are not shown**, and a contact's new identity key is trusted
  automatically, as presage's own stores do. A key change is not surfaced to you yet.
- **A Tiny server sees the shape of a call**, end-to-end encrypted or not: who is in it and when,
  who is speaking and how loud (the audio level is in clear), each frame's size and timing (Opus
  frames vary with what is said), and every address. It cannot hear the words.
- **A Tiny server can stop a call being end-to-end encrypted** by saying someone in the room
  cannot do it; the `off` and `start` frames are unauthenticated JSON from the server. tinyclient
  then blocks the call instead of carrying on (see above), so the server can deny an encrypted
  call but not quietly listen to one, unless you choose to carry on. A server that never offers
  `e2ee_calls` gets an "Encrypted to the server" call from the start, except on an Incognito
  host, in an `e2ee_required` room, or with "Only join end-to-end encrypted calls" on. The pin
  is per call: tinyclient does not yet remember that a community encrypted its calls before.
- **Unverified people can be impersonated by the server.** As the group's external sender it
  could add a member of its own, or put its own key under someone's name. A member it adds shows
  in the call's security panel; a key it swaps is caught by comparing safety numbers or the call
  code, and is flagged once you marked the real key verified. Until you compare, you trust the
  server's word for who is who.
- **Everyone in the call can open everyone's frames**, as in DAVE: sender keys come from the
  group's shared secret, so a member could forge audio as another member. Frames are not signed.
- **The identity key is not sealed** in the credential store; it is a file in the data directory
  (`e2ee/<account>/identity.key`, two lines of hex: the private and public key), protected by its
  permissions on Unix and by your profile's access list on Windows, like the cache. A key read
  off the disk lets someone pose as you in later calls until the people who verified you notice
  the change and verify again. Backing up or syncing your profile folder copies it.
- **Pictures leak their shape**: a sealed camera or screen-share frame keeps its NAL headers in
  clear, so the server sees frame types, sizes, keyframes and timing, as it does for audio.
- **macOS releases are signed ad hoc**, not with a Developer ID, and not notarized. Check that a
  download came from this repository's Releases page.
- **The window decodes images** sent by other people (PNG, JPEG, GIF, WebP, BMP through the
  `image` crate) in the GUI process.
