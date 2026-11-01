# e2ee core: notes, problems found in the spec, measurements

This is the working record of `crates/e2ee`, the client half of tinyserver's end-to-end encrypted
text and attachments (`docs/e2ee-text.md` in tinyserver, "the spec"). It says where the spec was
ambiguous or could not be followed as written, what was done instead and what wire change would
make it unnecessary; which dependencies were added and why; what was measured, with the machine;
and what is not covered.

The crate was written against the spec and then checked, read-only, against the server's
`crates/api/src/e2ee_text/` as it stood in the `hs-e2ee` worktree (commit `4a2de3b`) for wire shapes.
Nothing there was changed.

## 1. Where the spec is ambiguous, wrong or impossible

Each item: what, why, what the crate does, and the wire change that would fix it (if any).

### 1.1 The attachment id cannot be associated data (impossible as written)

Spec section 5: the blob's first chunk has "the attachment's id and the header as associated
data". The id is the server's, returned by `POST /v1/media` after the blob is uploaded. The blob
must exist before that. A client cannot seal with an id it does not have yet.

Done: `attachment::seal(key, context, ...)` takes `context: &[u8]` and the first chunk's associated
data is `header || context`. The caller passes whatever it knows when it seals. Opening needs the
same bytes. The id binding adds little in any case: the key and the `sha256` of the ciphertext are in
the authenticated message, and they name exactly one blob.

Proposed wire change, either: (a) `POST /v1/media` with `sealed=1` accepts a client-chosen `id`
(a UUID the client makes), so the id is known before sealing and the spec can stay as written; or
(b) the spec says the associated data is the header alone (pass `context = b""`). The crate works
with both; (b) needs no server change.

### 1.2 `kind` and `target_id` are not authenticated (gap)

The server stores `kind` and `target_id` beside the ciphertext, outside anything the sender signed.
A server (or anyone who can write the database) can turn an edit into a message, or point an edit
at another message of the same author: the server only checks "same user, not deleted". The edit
envelope (`{"v":1,"text":...,"attachments":...}`) has no target in it.

Done, no wire change: the MLS message's authenticated data is
`"tiny-e2ee-v1 msg" 0x00 kind 0x00 target_id`. `encrypt(conv, kind, target_id, ..)` sets it;
`decrypt(conv, ct, &Claimed { kind, target_id, .. })` refuses a ciphertext whose authenticated data
is not what the row says (`Error::AadMismatch`). The authenticated data travels in the clear in an
MLS `PrivateMessage` (it is authenticated, not encrypted), which is fine: the server already knows
both fields.

Not covered by this: `attachment_ids` on the post (the server's list), `reply_to` is inside the
envelope and is covered; the `sent_at` is the sender's clock and not trusted for order.

### 1.3 The proposals message has no defined shape in the spec

Section 5 says only that `handshake` returns `{seq, kind: "proposals", epoch, data}`. The server's
`e2ee_text::mls::batch` makes `data` a TLS vector of `MLSMessage`s (a variable-length prefix, then
the messages one after another). `Engine::handle_proposals` reads exactly that, and also the
DAVE form (one byte operation, then that vector, as `docs/calls.md` has it for calls) and one bare
`MLSMessage`, because the call code of this project uses the first two and a later server change
could produce the third. The server could pick one in the spec.

### 1.4 Key package, Welcome, commit and external sender encodings (spec silent)

* Key package: raw TLS `KeyPackage` (as DAVE and the server's `parse_key_package`).
* Commit: TLS `MLSMessage` holding a `PublicMessage`. The server reads the proposal references out
  of the bytes of that message. A test in `tests/groups.rs` reads them out the same way, from the
  bytes this crate produces.
* Welcome: TLS `MLSMessage` holding the `Welcome`. **The server's `welcome_recipients` parses an
  `MLSMessage`**, not a raw `Welcome` (the call code uses a raw one). This crate emits the
  `MLSMessage` and accepts either on input.
* External sender (`GET /v1/e2ee/conversations/{id}.external_sender`): the TLS `ExternalSender`
  base64, as the server produces it (credential `tinyserver`). A bare 65 byte key is accepted too. The
  pin is the 65 byte signing key; the credential is not compared.
* Cross-signature: DER ECDSA (the encoding MLS and the server's `verify_signature` use). The
  verifier here also accepts the 64 byte `r || s` form.

The spec should say these in section 5 once.

### 1.5 `bytes` in an attachment entry

The spec does not say whether `bytes` is the plaintext or the sealed length. This crate reads it as
the plaintext length (`attachment::open` can check it against what came out, and
`attachment::sealed_len`/`plain_len_of` convert). The server knows the sealed length itself.

### 1.6 A commit must apply exactly the proposals the server issued

The server (`apply` in `e2ee_text/mod.rs`) refuses a commit that does not name every issued
proposal, once each (`ProposalsChanged`). A client that refuses a proposal (for instance an Add of a
device it has no cross-signed record of) therefore cannot commit the others: the group stalls.
That is the safe failure (no ghost device joins), and `ProposalOutcome::rejected` says which
proposal and why (`UnknownDevice` is the retryable one: fetch the person's device list, call
`verify_devices`, feed the same proposals again). Nothing to change unless the server wants to let
a commit apply a subset and drop the rest after a retry; that is a server policy choice.

### 1.7 Section 10 says "a commit for a 1,000-device group is a few KiB"

Measured (section 4): the commit that adds 6 devices to a 994-device group is 382 bytes, but a
**Remove** commit in a 1000-device group built by adding devices is **117 KB**, and in a 100-device
group 12 KB. The cause is the tree shape: devices added by commits without an update path are
unmerged leaves, a path update has to encrypt to all of them. It is how MLS behaves; the number
shrinks as members commit (self-update) and the tree heals, and that was not done or measured here.
The claim holds for a healed tree, not for one grown by joins. The server's `COMMIT_MAX` is 256 KiB,
so a 1000-device group is near it right after growth.

Also from the measurements: a **Welcome carries the whole tree** (the ratchet tree extension), so
it is O(members): 4.3 KB at 10 devices, 28 KB at 100, 277 KB at 1000. The server's limits are 1 MiB
per Welcome and 4 MiB for all Welcomes in a commit, and each added device gets its own copy of the
bytes (one Welcome holds every added device's secrets). So a commit may add about 15 devices at 1000
members, and no Welcome fits the 1 MiB limit past about 3,700 members. `commit_pending` checks the server's limits
(`groups::limits`) and returns `Error::TooLarge` without leaving the commit in flight, instead of
posting something the server will refuse. The server has to batch Adds accordingly, a few at a time
in large groups; this crate cannot split a batch (a commit must name all issued proposals, 1.6).

### 1.8 The spec's "pin the external sender key" when the group's context changes

A commit can in principle change the group's external senders (a GroupContextExtensions proposal).
`handle_commit` refuses any commit that carries a proposal other than Add or Remove, and refuses any
Add or Remove that is not one of the server's proposals (a member's by value, or by reference to a
member's own proposal). So a member cannot change the pinned key or add or remove anyone itself.
Membership changes come only from proposals signed by the pinned key.

### 1.9 Messages and commits arrive in two feeds

`GET .../handshake` and the message list are separate. A message of epoch `e` that is read after the
commit leaving `e` can only be opened within `Config::max_past_epochs` (default 4) epochs, because
MLS discards older epoch secrets. The caller should open messages of epoch `e` before applying the
commit that leaves it when it reads both from the start. A message from a future epoch is
`Error::FutureEpoch` (apply handshake first).

### 1.10 A device cannot open its own messages

MLS gives a sender no way to decrypt its own application message. The caller keeps the plaintext
when it sends (its cache). `decrypt` on one's own message is `Undecryptable`.

### 1.11 Last-resort key package lifetime

OpenMLS rejects leaf lifetimes over 84 days (`has_acceptable_range`), and the server accepts at most
400, so the last-resort package is good for 84 days (regular ones too). A device that stays offline
longer than that has a last-resort package that expires; the server marks `low_keys` and the device
uploads new ones when it returns. Welcomes are staged with leaf lifetime validation off
(`skip_lifetime_validation`), because a long-lived group contains leaves whose lifetime ended: trust
here rests on the cross-signature chain, not on lifetimes.

## 2. What was decided where the spec was open

* **TOFU root.** With no device of a person known, the earliest device on the server's list
  (`created_at`, then id) is the root, whether or not it carries a cross-signature (a first device
  signed by one since revoked still looks like that). Every other device must chain to it. A server
  that is present at first contact controls this and nothing can be done about it (spec section 8).
* **Who may sign.** A new device is accepted if a cross-signature over it verifies against a device
  this call accepts: one trusted before that is still on the list with the same key, or one
  accepted earlier in the call. A device that is off the list no longer signs for anyone, even on
  the list that revokes it, so a stolen key cannot sign in a new device while the server drops the
  stolen one. The cost: a person who replaces their only device must approve the new one before
  revoking the old one; done in one step, it reads as a new identity (`Error::IdentityChanged`).
* **A new identity** (devices pinned, none now on the list verifies, the list is not empty, or every
  device was revoked and an unsigned one appears) is `Error::IdentityChanged`; nothing is trusted
  until `accept_new_identity`.
* **Trust is checked at three points:** an external Add (`handle_proposals`), every device a commit
  adds, and every member of a Welcome's tree. Unknown devices are returned in
  `Error::UntrustedDevices` so the caller can fetch and retry; a Welcome refused this way does not
  spend the key package (the crate puts it back).
* **Commits.** A commit in flight is kept whole in the state (`Engine::pending_commit`), so a restart
  or a lost answer can send it again; the copy of it in the feed settles it. The server's `409` is
  not a reason to abort: read the feed and give the `commit` item to `handle_commit`.
* **History shown to a new device.** Commits from before the Welcome's epoch are history for a
  device that joined later, not a rewrite. A commit shown for an epoch below ours that we did not
  apply, from a time we were in the group, **freezes the group** (`Error::EpochBackwards`); so does
  `observe_server_epoch` with an epoch below ours. The last 256 applied commits are remembered by
  hash (`Config::commits_remembered`); one older than that is taken for shown again, because it
  cannot be told apart.
* **A Welcome for an epoch already held** is `Error::Duplicate` and changes nothing; it does not
  freeze. (A first version froze the group: a feed read twice would have killed a healthy group.)
* **Wire format policy** is `PURE_PLAINTEXT` for handshakes (the server must read commits and the
  external proposals) and application messages are always `PrivateMessage`, padded to 128 bytes.
* **Safety number.** `safety::safety_number` reuses DAVE's pairwise fingerprint and displayable code
  from `davey` (the same construction `mediad` uses for calls). DAVE has one key per person, a person
  here has a set of devices, so each person's input is `SHA-256("tiny-e2ee-v1 safety" || user_id ||
  n || (device_id || signature_key) sorted by device id)`. 60 digits. It changes when either person
  gains or loses a device or a key changes. See the module documentation.
* **State.** One AES-256-GCM sealed snapshot (`0x01 || salt || nonce || ct`) under a sub-key
  `HKDF-SHA256(salt, state_key, "tiny-e2ee-v1 state")`, store name as associated data. The cache
  seal is the same with label `tiny-e2ee-v1 cache` and the row id as associated data. Operations
  that must not be repeated (making key packages, creating a group, committing, encrypting) return
  only after the write succeeded, so a crash cannot make a device use one message key twice.
  Receiving operations write too, but a failed write does not lose what they decrypted: `flush()`
  reports it.

## 3. Dependencies

Everything but the dev-dependency below is already in `Cargo.lock` at the version used.

| Crate | Why |
|---|---|
| `openmls` 0.8.2, `openmls_rust_crypto` 0.5.1, `openmls_traits` 0.5.0 | MLS. The versions `davey` and `mediad` link, so nothing is built twice. `openmls_traits` only for the `Signer` trait (the device key signs MLS handshakes and cross-signatures). No `js` feature (native only). |
| `aes-gcm` 0.10.3 | AES-256-GCM. Features `stream` (RustCrypto `aead::stream`, no new crate) and `zeroize` (no new crate). Used by attachments, the snapshot and the cache. |
| `p256` 0.13.2 | ECDSA P-256 for the device identity and cross-signatures. |
| `sha2`, `hkdf`, `subtle`, `zeroize`, `getrandom` 0.3 (workspace), `base64` 0.22 | As named. `zeroize`'s `derive` feature adds no crate (`zeroize_derive` was in the tree). |
| `serde`, `serde_json`, `thiserror` 2 | Snapshot and envelope; error type. All already in the tree. |
| `tempfile` 3 | The private 0600 temp file that removes itself, for a decrypted attachment, and the atomic file store. Already in the tree. |
| `davey` =0.1.5 | The safety number's fingerprint and code, as `mediad` does. The version `mediad` pins. |
| `proptest` 1 (dev only) | Property tests. The lock's only `proptest` is a git-sourced 1.10.0 that a registry requirement cannot reuse, so `Cargo.lock` gains registry `proptest` 1.12.0 and four crates under it (`bit-set`, `bit-vec`, `borsh`, `borsh-derive`) and `rand_xorshift` 0.5.0. Test builds only. |
| `libfuzzer-sys` 0.4 (only in `crates/e2ee/fuzz`) | The fuzz targets. Not in the workspace and not in `Cargo.lock`. |

No new cryptographic primitive anywhere. The only cryptographic code of this crate's own is
composition: the layout of what is signed and authenticated.

## 4. Measurements

Machine: AMD Ryzen 9 5900X (12 cores, 24 threads, 3.7 GHz base), 64 GB RAM, Windows 11 Pro
10.0.26300, rustc 1.99.1, `cargo run --release -p e2ee --example bench -- 10 100 1000` at commit
`fdb4221`. In-memory state (no store). One person per device. **The machine was shared with other
agents' builds, so the numbers move: two runs are shown, and per-message times differ by up to 2x
between them.** Times are wall-clock for one call. The group grows by commits of at most 8 devices (the
server accepts at most 100 Welcomes and 4 MiB of them in one commit).

Run A (quieter):

| | 10 devices | 100 | 1000 |
|---|---|---|---|
| identity key made | 130 us | 115 us | 114 us |
| key package made | 532 us (418 B) | 519 us | 614 us |
| external Add proposal taken | 753 us | 738 us | 1.01 ms |
| grow to N (all commits) | 29 ms (2) | 318 ms (14) | 6.05 s (126) |
| last commit: add k devices | 2.8 ms (k=8) | 1.9 ms (k=2) | 14.4 ms (k=6) |
| ... its commit / Welcome | 450 B / 4.3 KB | 247 B / 28 KB | 382 B / 277 KB |
| ... applied by another member | 0.5 ms | 1.4 ms | 16.3 ms |
| commit removing 1 | 2.5 ms (1.7 KB) | 4.8 ms (12.4 KB) | 33.3 ms (117 KB) |
| ... applied by another member | 1.3 ms | 2.8 ms | 16.6 ms |
| commit removing another | 1.7 ms (1.5 KB) | 4.6 ms (12.2 KB) | 31.2 ms (117 KB) |
| message encrypt, 1 / 4 / 15 KiB | 202 / 189 / 201 us | 753 / 608 / 615 us | 8.08 / 6.95 / 6.97 ms |
| message decrypt, 1 / 4 / 15 KiB | 280 / 277 / 287 us | 794 / 711 / 650 us | 7.10 / 7.32 / 6.06 ms |
| snapshot sealed (size) | 0.3 ms (86 KiB) | 2.6 ms (899 KiB) | 22.6 ms (8.8 MiB) |
| snapshot opened | 0.3 ms | 1.7 ms | 15.0 ms |

Run B (the same code, a few minutes later, busier machine): message encrypt / decrypt 1 KiB: 260 /
438 us (10), 1.74 / 1.54 ms (100), 11.8 / 11.2 ms (1000); commit removing 1: 7.6 ms, 66 ms, 99 ms;
snapshot sealed 0.6 ms, 4.8 ms, 41 ms. Same sizes in both runs.

What the numbers say, and no more:

* The cost of a message does not depend on its size in this range (1 to 15 KiB are the same within
  noise) and does depend on the group: it is dominated by OpenMLS writing the secret tree back to
  its storage for every message, which grows with members and with the epochs kept
  (`max_past_epochs`). At 1000 devices that is milliseconds per message, and persisting a snapshot
  after every message (the default when a `Store` is attached) adds the snapshot's 20 to 40 ms.
  A caller with big groups should attach the store and use `flush()` on its own schedule for
  receiving, and lower `Config::max_past_epochs`. Sending always writes (see section 2).
* Attachments, 16 and 128 MiB, 64 KiB chunks, AES-256-GCM STREAM, SHA-256 included on open: run A
  seal 639 and 654 MiB/s, open 902 and 914 MiB/s; run B seal 451 and 412 MiB/s, open 685 and
  587 MiB/s.
* Memory while sealing or opening an attachment: 128 KiB peak whatever the size, measured with a
  counting allocator over 1, 8 and 64 MiB (`tests/attachment_memory.rs`).
* AES-GCM is the hardware path (the `aes` crate's runtime detection); no figures for a machine
  without AES-NI.

## 5. Tests

`cargo test -p e2ee`: 90 tests in 8 files, all pass (about 30 s, 22 of it the 64 MiB memory test).
`cargo clippy -p e2ee --all-targets` is clean; `cargo fmt -p e2ee -- --check` passes (this crate is
formatted with rustfmt's defaults; the rest of the repository is not, and was not touched).

| File | Tests | What |
|---|---|---|
| `groups.rs` | 28 | Real openmls groups: 3 and 20 devices through add, remove, commit and epoch change; removed device reads nothing later, new device nothing earlier; replay, tamper (bytes flipped, cut, forgeries do not spend the real key), wrong sender, kind or target relabelled, edit authorship; ghost device (no cross-signature, forged signature, borrowed id); proposals not from the pinned key; Remove of a non-member; the three proposals encodings; pending Remove blocks sending, the removed device cannot commit and is evicted; two commits one epoch; repeated and own commits; commit confirmed by the feed after a lost answer; commit in flight survives a restart; server key change (also in the Welcome) and re-pin; epoch backwards (server shows lower, rewritten history); a commit that adds an unknown device is held until known; a Welcome with unknown members does not spend the key package; last-resort key package serves two groups; forged commits (new key under the old name, new credential, new external sender, an Add by value); the commit and Welcome shapes the server checks, and the server's size limits; replayed Welcome; commits older than the remembered ones; key package fields |
| `identity.rs` | 11 | Cross-signature bytes and verification (and every way to misapply one); first device, chains, order; invented, forged, replayed and borrowed devices; key changed; revocation; new identity; malformed entries; safety number |
| `attachments.rs` | 15 | Round trip at 0, 1, 15-17, 65535, 65536, 65537, 131071-131073, 1 MiB and 10 MiB; the blob rebuilt by hand from AES-256-GCM to pin the nonce layout; truncation at every kind of boundary; reorder, repeat, drop, append; bit flips; wrong key, wrong context; header size bytes; expected hash and length; hash checked before decrypting; temp file private, removed on every failure; stale temp sweep; failing reader and writer; keys |
| `attachment_memory.rs` | 1 | Peak heap 128 KiB over 1, 8 and 64 MiB, seal and open |
| `kat.rs` | 4 | **84 known-answer vectors** against the exact `Aes256Gcm` the crate uses: 50 NIST CAVP `gcmEncryptExtIV256` (96-bit IV, 128-bit tag, many lengths of plaintext and AAD), 10 NIST `gcmDecrypt256` (4 pass, 6 `FAIL`), 24 Wycheproof `aes_gcm_test.json` (AES-256, 96-bit IV, 128-bit tag; 14 valid, 10 invalid). Downloaded from csrc.nist.gov and C2SP/wycheproof, embedded in `tests/vectors/mod.rs` |
| `envelope.rs` | 10 | The spec's example; message and edit round trips; unknown fields; bad types (28 cases); version; UTF-8; the 16 KiB limit both ways; counts and lengths; deep nesting |
| `props.rs` | 11 | proptest: envelope round trip; arbitrary chunk-boundary and short reads and writes through seal and open; damaged blobs always refused; `sealed_len` inverts; never panicking on arbitrary bytes into the envelope decoder, the blob parsers, the small parsers, and every MLS parser; hostile varint prefixes; damaged real handshakes, Welcomes and ciphertexts do not stop the group |
| `store.rs` | 10 | State survives a restart; a failed write stops encrypt and key package creation, not a decrypt; authenticated, bound to name and key; no plaintext in the blob; caller-kept snapshot; file store; cache sealing bound to its row; redacted debug; `Send` |

The proptest run used 40,000 cases per parser test once, with no escaped panic (ordinary runs use
300 to 2,000 cases). `tls_codec` 0.4.2 asserts on a bad variable-length prefix and OpenMLS has a
`debug_assert!` on a failed AEAD check; every parse of server bytes therefore runs under
`catch_unwind` (`wire::guard`), and the tests print the panics the guard caught (about 950 in one run).
This needs `panic = "unwind"`, which is the default; with `panic = "abort"` a hostile server could
crash the client. The server's code has the same guard.

`crates/e2ee/fuzz` has four cargo-fuzz targets (`envelope`, `blob_header`, `sealed_blob`,
`mls_message`), outside the workspace (own `[workspace]`). **They compile (`cargo check`); they were
not run**: this machine has only the stable toolchain and no clang. Run
`cargo +nightly fuzz run mls_message` where those exist, after copying the root `Cargo.lock` next to
the fuzz `Cargo.toml` so it builds the same versions as the workspace.

## 6. Not covered

* No test against the real server (it is another agent's work in another worktree); wire shapes were
  checked by reading its code and by parsing this crate's output the way that code does.
* Network partitions beyond what the tests build (two committers, a lost answer, a restart, a feed
  shown twice). No property test of arbitrary interleavings of commits and proposals across members.
* Healing the tree (self-updates after joining), so the large Remove commits of 1.7 stay large.
* Past-epoch handling beyond `max_past_epochs`, and reordering of messages beyond
  `out_of_order_tolerance` (64): such a message is `Undecryptable`.
* A client restored from an old backup of its own state: it reuses message keys it already used and
  nothing here can tell. Do not restore an engine's state from backup; make a new device.
* Memory hygiene: the keys inside OpenMLS's in-memory storage are plain heap bytes and are not zeroed
  when the engine is dropped; the snapshot's plaintext is built in a zeroing buffer but its
  intermediate strings are not. Device and attachment keys, the state key, the cache key and the
  chunk buffers are zeroed.
* No ciphertext padding beyond MLS's (128 bytes); sizes of attachments are visible to the server.
* The local cache helper seals a value; the schema, the key's place in the keychain and the
  retention of decrypted text are the caller's.

## 7. Needs independent cryptographic review

* The device chain verifier's rules (section 2: who may sign, the TOFU root, `IdentityChanged`).
* The safety number: DAVE's construction composed with the per-person digest of a device set.
* The sealed blob: STREAM BE32 with the AAD on the first chunk only (as the spec says); `context`.
  Is first-chunk-only enough where all-chunks is as cheap? A reviewer may prefer all chunks.
* The authenticated data that binds `kind` and `target_id` (1.2).
* The commit and Welcome vetting rules in `groups.rs` (what a member may and may not commit; the
  roster check; the key package given back on a refused Welcome, which writes OpenMLS's storage
  directly).
* The state snapshot's sealing (HKDF sub-key per blob, random nonce, store name as AAD) and the
  rule that encrypt returns only after the write.
* The group-state machine under partition, and the rollback/freeze rules (`joined_epoch`, the
  remembered commit hashes).
* Everything in OpenMLS 0.8.2 and RustCrypto, which this crate relies on and did not audit.
