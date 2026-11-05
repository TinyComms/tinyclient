# Encrypted text in the Tiny adapter

How tinyclient uses `crates/e2ee` against a tinyserver that speaks encrypted text (tinyserver's
`docs/e2ee-text.md`, "the spec"). The crate does every piece of cryptography; this is the part
with a network, a disk and a window. Code: `crates/chatd/src/adapters/tiny/e2ee.rs` (the device),
`sealing.rs` (files on the way out), `blobs/fetch.rs` (files on the way in), `messages.rs` (where
rows become messages), and the Encryption page of Server Settings in the shell.

## What lives where

| What | Where |
|---|---|
| The state key and the cache key | In the account's keychain item, inside the Tiny session JSON (`e2ee`), written before anything is sealed with them. Never on disk. |
| The cache key of an account signed out with its history kept | The keychain item `Tiny kept history` (`{"<user> <server>": key}`), until that account signs in here again and takes it back. |
| The engine's sealed snapshot (identity key, trust store, MLS groups) | `<support>/e2ee/Tiny-<user>/text/device.state` |
| Opened messages, sealed one by one under the cache key with the message id as associated data | `.../text/cache-<conversation>.log` |
| Handshake positions, people met, verified safety numbers, the recovery key's check value | `.../text/notes.json` (nothing that opens anything) |
| A sealed file waiting for its upload | `<support>/e2ee/outgoing/.tnya-*`, removed after the send, swept after an hour |

The `text` and `outgoing` directories are made owner-only: 0700 on Unix, and on Windows a
protected access list with only the owner and SYSTEM. Signing out revokes the device on the
server and deletes its state, and by default keeps the opened messages ("Keep my messages on this
device"): the cache stays sealed and its key moves to `Tiny kept history`, so the next sign-in of
that account here (a new device) reads them. "Erase" deletes the directory and the kept key.

## Flows

- **Enrolment.** When the account starts, chatd asks `GET /v1/capabilities` for `e2ee_text`, opens
  or makes the engine, and registers the device. A person's first device is trusted on first use.
  A later one gets `cross_signature_required` and shows a **link code** (`tiny-device-` and the
  device id and signing key, base64url) with its fingerprint; the person pastes the code into
  Encryption on a device they already have, which cross-signs and registers it. The new device
  hears `e2ee.device` and finishes. Then 50 key packages and a last-resort one go up, and are
  topped up on `e2ee.keys_low` or below half.
- **A conversation.** `sync` reads `GET /v1/e2ee/conversations/{id}`, verifies the device list of
  everyone in `members` and `pending`, pins or checks the external sender key and the epoch,
  starts the group if nobody has (`POST .../group`), applies the handshake feed in order (an Add
  for a device not yet verified makes it fetch that person's list and try once more), and commits
  what is pending, with the Welcome for every device it adds. It runs on `e2ee.handshake`, before
  every send, and when a message is from an epoch ahead.
- **Sending.** Never plaintext to an encrypted conversation: if this device is not ready, the send
  is an error. The envelope is encrypted (`kind` and `target_id` in the authenticated data) and
  posted; the plaintext is kept in the cache under the server's id, since a device cannot open its
  own message. An edit is a `kind: edit` ciphertext that keeps the message's attachments.
- **Reading.** Rows of an encrypted conversation are opened (or read from the cache) oldest epoch
  first; an edit is folded into its target if the target's author sent it. The server's `body` is
  never read for such a row. What does not open reads "Can't be decrypted on this device" with
  `Seal::Undecryptable`. A plaintext conversation is `Encryption::Plain` until this device is
  ready and the server allows encryption: it then asks `POST /v1/conversations/{id}/e2ee`. Older
  messages stay in the clear and the header says they can still be read by the server. A
  conversation that was encrypted from the start does not say that.
- **Files out.** PNG, JPEG and WebP are decoded with limits (16,384 px a side, 100 MP, 512 MiB) and
  turned upright, then written again (JPEG stays JPEG at quality 88, the others become PNG), which
  drops EXIF, XMP, IPTC and GPS. "Send as file" and originals skip it. The file is sealed under a
  fresh key with an attachment id chosen now; the first chunk's context is
  `conversation_id ‖ attachment_id`, and the upload names the id (`sealed=1`, `id`).
- **Files in.** The cache's fetch asks the adapter, which answers with the key, hash, length and
  context from the message. The sealed bytes' SHA-256 is checked before anything is decrypted, the
  file is opened into a private temp file, and its first bytes must be what the message says (a
  picture must be the picture format it names; a program must be named as one). Saving or opening
  an attachment marks it as downloaded: `Zone.Identifier` with `ZoneId=3` on Windows,
  `com.apple.quarantine` on macOS, so SmartScreen and Gatekeeper look at it.
- **Safety numbers.** Encryption lists the person's devices (one the server lists without a chain
  to a trusted device is shown as not approved), everyone met in encrypted conversations with the
  60-digit safety number (`e2ee::safety`), "Mark as verified", and a warning when a verified number
  changed or a person has a new identity (their new devices are not let into any group until the
  person accepts it).
- **Adding a device with its history** (servers with `e2ee_device_link`; the contract is tiny's
  `research_notes/device-link-protocol.md`, the crypto `e2ee::link` and `e2ee::archive`, the
  flows `e2ee/linking.rs`). The new device makes a link and shows a `tiny-link-1.` code (text and
  QR). The person gives it to a device they have, which answers with an ephemeral key; both show
  six digits. The old device shows the new one's label, system and fingerprint and asks the
  person to type the digits and choose the history (none, 7 or 30 days, all, with sizes). Then it
  cross-signs, packs its cache (and verified marks) into an archive under a fresh key, uploads
  it, and sends the key and signature on the channel. The new device registers with the
  signature, downloads, checks the SHA-256 and the archive, merges it (never overwriting, only
  conversations the person is in, rows marked `imported_from`), says done, and both delete the
  link. Any failure, a cancel or wrong digits sends `abort` and deletes the link. A server without
  the feature keeps the paste-the-code approval, and a `tiny-device-` code pasted where a link code
  goes is approved that way. Every device of the person shows "New device added" for a device it
  did not add itself, with Revoke, from the next `e2ee.device`.
- **The person's own devices** (servers with `e2ee_self`; `e2ee/siblings.rs`). Each device joins
  the person's own encrypted conversation (`POST /v1/e2ee/self`), whose group is exactly their
  active devices; the server hides it and deletes its messages after `e2ee_self_ttl_secs`. A
  device that cannot open a message asks there (`want`); a sibling that opened it answers from
  its cache (`have`), kept here as `imported_from` that device, never overwriting a row. Marking a
  safety number as verified, or unmarking it, goes there too. Read positions need nothing: they
  are the server's, per person. On start a device reads what was said while it was away. Messages
  there count only from the person's own other devices, and never show as a conversation.
- **Backup file** (`e2ee::backup`, `e2ee/backup.rs`). Encryption's "Save a backup…" writes the
  cache and verified marks as a `TNYB1` file: an `e2ee::archive` under a key from Argon2id
  (64 MiB, 3 passes) over a passphrase of at least 12 characters, with the salt and cost in a
  header that is the archive's associated data. "Import a backup…" on a device of the same person
  merges it like a device link's archive. No server ever keeps a backup.
  Export sorts sealed cache values through a temporary SQLite index in the private encryption
  folder, opening one message at a time. The index contains ids and cache times, but no message
  plaintext or keys, and is removed when export finishes. Import validates every message schema
  before writing any rows, so a malformed later record cannot leave an earlier row imported.
  Sibling replies measure the actual encoded envelope, fragment large rows with a checked hash,
  and page through missed replies after reconnecting; duplicate sibling publishers cannot fill
  the partial-row budget with copies of the same requested row.
- **Recovery key.** 128 bits from the system's random source, shown once as 26 base32 characters
  in six groups; only an HKDF check value is kept, so it can be checked later. There is no key
  backup yet (the crate has none): the page says the key backs nothing up.

## Testing

`cargo test -p chatd --lib -- e2ee sealing fetch messages` runs the unit tests. The end-to-end
test `adapters::tiny::e2ee::live` is ignored unless `TINY_E2EE_SESSIONS` holds a JSON array of five
Tiny sessions on one tinyserver built from the encrypted-text branch: Ada, Bea, Cyd, then two more
of Ada's. Sessions were made with tinyserver's own `db` functions (`owner_invite`, `sign_in`,
`create_invite`, `open_session`) against Postgres 17 with the `app_owner` and `app_runtime` roles,
and the server started with an OpenID provider that answers discovery only. Then:

```sh
TINY_E2EE_SESSIONS="$(cat sessions.json)" cargo test -p chatd --lib e2ee::live -- --ignored
```

It covers: first device trusted, second waits for approval and is approved by link code, an
encrypted direct message started and joined, the server holds no body and no plaintext in the
ciphertext, the other person and the sender's other device read it, a reply, an edit folded in and
an edit of another's message refused, a sealed picture with EXIF cleaned, uploaded under its own id
and opened by the other side, a device approved later that reads nothing from before it joined and
reads what comes after, matching safety numbers, a revoked device that cannot open what comes after,
and the recovery key.

## Not done

- Encrypted messages now retain the sender's authenticated millisecond date in the sealed cache,
  device-link archives and the local display order. The server still sees only its rounded date.
  Older cache rows discarded that sender date and cannot reopen already-consumed MLS ciphertext;
  they continue to use the rounded server date, including when imported from an older archive.
- chatd's message store (SQLite) keeps the opened text of encrypted conversations in plain text,
  as it keeps every network's messages; the media cache keeps opened files. Only the e2ee cache is
  sealed. Sealing the store is follow-up.
- No key backup to the server and no restore from the recovery key (the backup file is the
  person's own).
- Device linking: no OS unlock (Windows Hello, Touch ID, a phone's biometrics) before the old
  device approves; the person types the six digits instead. Messages opened before this release
  carry no date, so "last N days" leaves them out (they go with "all").
- The community root key (`/v1/e2ee/community`, `e2ee.community`) is not pinned or checked yet.
- Ratchet trees are always inside the Welcome; the tree endpoint (`GET .../tree`, `ratchet_tree`
  on commit) is not used, so very large groups hit the Welcome limit sooner.
- Reactions stay plaintext (spec v1). Thumbnails, BlurHash and Padmé size padding are not sent.
- GIFs and other files are not re-encoded; video metadata is not stripped.
- A safety number is per pair of people; there is no QR verification, and no inline notice in the
  conversation when a verified person's devices change (it shows in Encryption).
- Creating an encrypted channel from tinyclient (the client creates only voice rooms today).
