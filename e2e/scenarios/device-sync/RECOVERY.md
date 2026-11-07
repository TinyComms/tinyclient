# Catch-up recovery proof preparation

This is a prepared harness and run procedure. It has not executed the recovery
scenario and supplies no desktop or Android pass. `scenario.json` remains
`release: false`; the existing coordinated IPC result cannot certify this new
recovery case. macOS and iOS are outside this proof.

Preserve the original failed mobile fixture, its profiles, public diagnostics,
wire, cursors and raw videos. Do not revoke, stop, restart, reset or reconfigure
that fixture. Earlier scratch profiles used memory keys: restarting their
controls cannot establish persistence. A later persistence check requires a
fresh fixture with durable keys and exact final-source builds.

## Owned handshake delivery gate

The throwaway fixture's existing `linkproxy.py` reads this stanza from its
private control JSON. Use actual owned IDs; the UUIDs below only illustrate the
schema. Atomically replace the control file using a temporary sibling file.
Retain any other intentional fault controls.

```json
{
  "handshake_gate": {
    "id": "55555555-5555-4555-8555-555555555555",
    "conversation_id": "11111111-1111-4111-8111-111111111111",
    "device_ids": [
      "22222222-2222-4222-8222-222222222222",
      "33333333-3333-4333-8333-333333333333"
    ],
    "state": "paused",
    "expires_at": 1793910000,
    "max_wait_seconds": 90
  }
}
```

The attach-only phase driver is documented in [RECOVERY-RUN.md](RECOVERY-RUN.md).
It starts/stops no app and retains explicit checkpoints for recorded native actions.

Set `expires_at` to the current Unix time plus the bounded duration, rather than
copying the example timestamp. The hard maximum is 120 seconds, with at most
one pending request per named device and at most two named devices. The first
matching request arms one deadline shared by both readers. Expiry returns 503
without forwarding, and retries remain refused until explicit release. A run
that expires must fail its gate assertion; it cannot count as the intended
deadlock reproduction.

Only `GET /v1/e2ee/conversations/{exact-id}/handshake` requests whose single
`device_id` query value matches a named owned device are held. Authorization
headers are forwarded normally and never matched, stored or logged. A query ID
is routing scope, not evidence of identity. Other devices, tree/group-info
reads, commits and message routes continue normally. The gate forwards no HTTP
request bytes to the service until release; it does not buffer or alter MLS
handshake bodies.

Release by changing **only** `state` to `released` in the exact same stanza.
Deleting the stanza, writing malformed JSON, changing the gate ID/scope or
extending its deadline cannot release requests or let retries bypass an armed
gate. Do not use the existing `Linker.proxy(...)` helper while gated: that
helper replaces the whole control file. Observe `handshake_gate_held`,
`handshake_gate_released`, `handshake_gate_timeout` and
`handshake_gate_capacity` records in `wire.jsonl`. The normal client HTTP timeout
is shorter than this gate. Retries while the first request is pending receive an
intentional 503 and `handshake_gate_retry_refused`; this is expected fault setup,
never successful delivery or cryptographic proof. It occupies no additional slot,
forwards nothing and cannot extend the shared deadline. Held events contain public
gate/conversation/device IDs, never tokens, bodies, keys or query strings.

## Deterministic red sequence

Execute only after reviewed final engine, adapter, service and GUI artifacts
are ready and the coordinating agent authorizes a new runtime fixture.

1. Allocate a fresh owned state/profile namespace, unused ports and labeled
   database container. Record committed source markers and SHA-256 for every
   app/helper/server artifact. Never relabel earlier binaries. Start raw
   recordings before the first action. Keep all local secrets private.
2. Start original Ada A and Bea, establish ordinary directory trust and an
   authenticated encrypted group. Save known text, a picture and the actual
   downloaded picture bytes/hash/length on both devices. Use the post-cleaning
   downloaded bytes as the reference. Read-only server acceptance queries are
   corroborating evidence; a fabricated message ID or cache delay is no oracle.
3. A approves X through the normal link/approval flow; let X genuinely join the
   existing group through strict Welcome. Record public old roster and epochs.
   A and Bea must already have authenticated X before their delivery is paused.
4. Arm the gate for **only A and Bea** in the target conversation. Wait for a
   held event for each and retain their authenticated pre-gate feed cursors.
   Confirm neither reader has already processed the later Add. Keep X flowing.
5. X approves nested device Y through the ordinary approval flow and commits
   Add Y. Corroborate the actual accepted commit, its generation/epoch and
   Y's real Welcome; do not infer acceptance solely from client optimistic UI.
6. Revoke Y, then revoke its approver X while both original readers remain
   held. Record the fresh ordinary directory result showing both inactive.
   Do not revoke either original A or Bea.
7. Explicitly release the exact gate and verify released events for both.
   Group metadata must show the actual advanced epoch. A and Bea must refuse
   the historical Add Y because its approval chain is no longer current.
   Their authenticated cursors/rosters must not falsely advance past it.
   Attempt real sends and verify no accepted fresh ciphertext/message exists;
   retain the bounded visible/IPC error. This is the cryptographic deadlock,
   independent of a slow cache or missing UI repaint.

If a reader reached Add Y before revocation, or if the deadline/capacity fault
fired, this attempt does not prove the required red sequence. Preserve that
failed attempt and start a separately owned fresh fixture when authorized.

## Repair and positive proof

1. Original active A initiates the explicit conversation repair action and
   completes the platform's established native owner verification. A canceled
   verification must publish no binding. Record the actual GUI action; an
   early IPC-scoped check must be labeled IPC and cannot count as GUI proof.
2. Save the public repair response and immutable chain. Use
   `lib.recovery.inspect_repair_response(response, pinned_external_key)` to
   independently check canonical bytes, padded base64, P-256 signature, head
   hash and unchanged external-sender key. The pinned key comes from independent
   existing trust, never from the binding. This helper does **not** prove the
   signer's directory status, prior authenticated membership or engine
   acceptance. Capture those actual client results separately on A and Bea.
   Confirm a distinct internal group UUID and immediate successor generation.
3. Bea independently refreshes the signer directory, keeps its pins, validates
   the public binding against its old authenticated roster and joins via strict
   Add/Welcome. Record actual fresh-group roster and successful decryption on
   both original devices. A public server membership list alone is insufficient.
4. Send fresh text and a new picture using the actual desktop GUI; verify the
   real accepted message and exact downloaded bytes at the peer. Read the
   pre-repair saved text and picture bytes again. Cached history surviving
   does not imply all previously unread historical ciphertext was recovered.
5. Prove X and Y cannot decrypt the fresh-generation text or attachment, using
   those devices' preserved private state and actual ciphertext. A missing
   message in their feed or an HTTP permission denial alone is insufficient.
6. After repair, submit omitted-generation and explicit old-generation writes
   and require 409 `generation_changed`. Cover create, commit, message, edit,
   reaction, tree upload and proposal issuance through their real routes.
   Include stale old requests with equal epochs. Compare a fresh generation0
   control where legacy omission still works. Retained historical handshake/tree
   reads must explicitly select generation0 and must not issue proposals.
7. Link an ordinary new sibling and the real Android participant after repair;
   each joins the current generation through strict Welcome and existing trust
   rules. Use `hostId: local`, `deviceId: Pixel_7`, `device_open` and the bound
   device CLI for all Android UI. Do not drive UI through adb. Record its actual
   native screen; a host phone-sized viewport proves layout only. Make genuine
   Android GUI sends and verify receipt and attachment bytes on the desktops.
8. Restart only verified owned durable-key participants from this **fresh**
   fixture and recheck head, retired outbound refusal, saved bytes and fresh
   sends. Preserve profiles. Never infer durable recovery from a process using
   memory keys or a control helper restart.

Generation contract: optional numeric JSON `generation` on group create,
commit/message/edit/reaction, and query `generation` on group-info, handshake
and tree; omission means legacy0. Group-info adds `generation`, `group_id`,
`head_hash`; active mismatch returns 409 `generation_changed`.
`POST .../repair {binding}` returns `{generation,group_id,head_hash,binding}`
(201 new, 200 identical retry).
`GET .../generations?after=0&limit=100` returns
`{generation,group_id,head_hash,bindings:[...]}`, ordered successors.
Handshake/message/reaction rows include generation and internal group UUID.

## Evidence and limits

Keep private runtime artifacts under the coordinating workspace's
`_proof/device-link/` namespace. Retain exact source/artifact hashes, public
pre/post rosters, cursor and epoch observations, the immutable binding, all
failed attempts, exact byte comparisons, raw native recordings and step files.
Use the independent vector in `fixtures/repair-binding-v1.json` only as a test
fixture; it is not a runtime binding.

Run the chosen-marker server scan against a fresh database dump, service data,
logs, public binding ledger and proxy wire. Include fresh fixture device-private
key needles and group-secret needles collected locally without publishing or
logging them; report only needle kinds and offsets. Positive controls must show
each chosen needle existed in the owned local input before a clean result can
count. A clean chosen-marker scan is not a general secrecy audit. Never export
private snapshots into the public binding artifacts.

Keep existing motion, recording duration and caption checks. Report every
missing native recording or unsupported platform as a gap. No all-platform or
release-ready claim follows from an IPC run. Restore owned emulator settings
after the recording; stop only processes whose stored creation identity,
executable and owner match. No process-name sweeps or recursive profile cleanup.

Harness-only verification (does not start the real fixture):

```powershell
python -m unittest discover -s . -p 'test_*.py' -v
```
