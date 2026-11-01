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
| The engine's sealed snapshot (identity key, trust store, MLS groups) | `<support>/e2ee/Tiny-<user>/text/device.state` |
| Opened messages, sealed one by one under the cache key with the message id as associated data | `.../text/cache-<conversation>.log` |
| Handshake positions, people met, verified safety numbers, the recovery key's check value | `.../text/notes.json` (nothing that opens anything) |
| A sealed file waiting for its upload | `<support>/e2ee/outgoing/.tnya-*`, removed after the send, swept after an hour |

The `text` and `outgoing` directories are made owner-only: 0700 on Unix, and on Windows a
protected access list with only the owner and SYSTEM. Signing out revokes the device on the
server and deletes the directory.

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
  `Seal::Undecryptable`. A plaintext conversation is `Encryption::Plain` and says "Not end-to-end
  encrypted"; it never becomes encrypted.
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

- chatd's message store (SQLite) keeps the opened text of encrypted conversations in plain text,
  as it keeps every network's messages; the media cache keeps opened files. Only the e2ee cache is
  sealed. Sealing the store is follow-up.
- No key backup and no restore from the recovery key; no device linking with history transfer.
- The community root key (`/v1/e2ee/community`, `e2ee.community`) is not pinned or checked yet.
- Ratchet trees are always inside the Welcome; the tree endpoint (`GET .../tree`, `ratchet_tree`
  on commit) is not used, so very large groups hit the Welcome limit sooner.
- Reactions stay plaintext (spec v1). Thumbnails, BlurHash and Padmé size padding are not sent.
- GIFs and other files are not re-encoded; video metadata is not stripped.
- A safety number is per pair of people; there is no QR verification, and no inline notice in the
  conversation when a verified person's devices change (it shows in Encryption).
- Creating an encrypted channel from tinyclient (the client creates only voice rooms today).
