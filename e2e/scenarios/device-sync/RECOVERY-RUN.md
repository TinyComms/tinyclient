# Attach-only recovery driver

`recovery_run.py` coordinates the deterministic red sequence in `RECOVERY.md`.
It never starts, installs, stops, restarts or cleans up an app/server/profile.
Each phase is explicit, ordered and recorded once in `recovery-journal.json`.
Every summary remains `release:false`, `native_proof:false`. Harness unit tests
are not a runtime pass. This driver does not post lab or release statuses.

## Fresh fixture preparation by the coordinating agent

Wait for the coordinating agent's authorization, final artifacts and source
markers. Preserve every earlier fixture, including the failed mobile fixture.
Reserve a previously absent namespace with:

```powershell
python recovery_run.py reserve --parent $fixtureParent --name recovery-$runId
```

This creates only that directory and an ownership marker. It refuses existing
directories and names outside `recovery-*`. Start the separately prepared fresh
service/database/recorders and owned app participants using the existing fixture
tools. Pin the final app/helper/service SHA-256 and their actual committed source
markers. Record the app source and the harness source independently if they
differ; changing the harness does not change an already built app's provenance.
Keep lifecycle creation-time/executable/owner identities with that fixture.
Use `WinLocal.start(fresh=False)` only after checking these are newly allocated
profiles. The attach driver itself has no start or cleanup command.

Scratch profiles set `TINYCLIENT_DIR`, so `session::enabled()` disables the
normal keyring. Newly generated keys otherwise live only in the process's
`scratch_secrets()`. Before *any* device first starts, use
`lib.recovery_runtime.fixture_sessions(original_seed)` to produce unique
32-byte state/cache wrapping keys in each new session's `e2ee` object. Save
the returned sessions once in an owner-only private fixture file; do not print
them, put them in evidence, overwrite them, or copy keys between devices. The
helper refuses sessions already containing `e2ee`. It generates wrapping keys,
not device identity keys. Retain the exact same private per-device sessions for
later restarts. Label that check **fixture-key persistence**, not Windows
Credential Manager persistence. The normal Android app has its own established
app-private persistence. Never change normal profile/keyring settings to make a
scratch fixture persistent.

The private manifest supplied by the coordinator contains:

| Field | Required value |
| --- | --- |
| `state` | Absolute reserved `recovery-*` directory |
| `seed` | Private provisioned sessions file inside that directory |
| `base_url` | The fixture proxy's exact loopback HTTP base URL |
| `observer_url` | Direct fixture service's exact loopback HTTP base URL, bypassing the held delivery proxy |
| `proxy_control`, `wire` | Exact owned control JSON and `wire.jsonl` paths inside `state` |
| `conversation_id` | Canonical encrypted conversation UUID |
| `devices` | Role-to-seed-session mapping for `a`, `bea`, `x`, `y`; four distinct profiles |
| `key_persistence` | `unique-fixture-wrapping-keys` |
| `sources` | `client` and `server` full committed source SHAs; add the separate `harness` SHA after its commit |
| `artifacts` | `chatd`, `tinyclient`, `dlctl`, `tinyserver`, plus other actual helpers: each `{path,sha256}` |

No credentials or keys belong in this manifest. They remain in its private
`seed` file. Artifact hashes are checked on each attach; URLs/fixture paths and
profile names are bounded. Starting/recording processes remains the coordinator's
job, with the existing lifecycle identity guards.

## Baseline and native checkpoints

On A/Bea establish ordinary trust, a genuinely encrypted conversation, saved
text and a picture. Download the real picture on both originals, compare their
normalized bytes and retain that reference. Approve/link X normally and let it
join through strict Welcome. Send a *new X-authored message after its Welcome*,
then observe actual decryption on A, Bea and X. Enroll Y but leave it awaiting
approval. Stage its link UI before arming if useful; approving it must happen
while both originals are actually held.

Create a public baseline JSON with `conversation_id`, `messages:[{id,text}]`,
`picture:{id,name,sha256,bytes}`, `x_join_message_id`, and `x_join_text`.
The picture hash/length must be those actual normalized original-device downloads.
The prepared phase fetches it again through each real device's attachment path
and verifies exact bytes. It checks actual encrypted message state and text,
records public generation0 metadata and the original feed cursors, and never
exports sealed engine state or session material.

In these commands `$m` is the private manifest and `$out` is a **new** private
evidence directory. Start raw native recordings before baseline actions.

```powershell
python recovery_run.py prepared --manifest $m --out $out --history $baseline
python recovery_run.py armed --manifest $m --out $out
python recovery_run.py held --manifest $m --out $out
```

`held` checks actual proxy held events for both original device IDs. It does not
wait indefinitely; invoke it after seeing both events. The shared hard deadline
is 120 seconds. Normal 10-second HTTP timeout retries get an intentional 503 and
`handshake_gate_retry_refused`: expected setup, not a successful handshake. No
retry forwards or extends the deadline. An expiry invalidates the attempt.
Do not use `Linker.proxy()` while gated because it replaces all controls.

**External checkpoint:** X approves Y through the real ordinary link UI/native
owner flow. Complete strict Add/Welcome. X sends a new encrypted control and Y
actually decrypts it. The following command corroborates the accepted commit,
Y-targeted Welcome and that fresh X-authored server ciphertext. It reads the
service directly, so the observer never occupies an original reader's slot.

```powershell
python recovery_run.py nested --manifest $m --out $out --message-id $newXtoYMessage
python recovery_run.py revoked --manifest $m --out $out
python recovery_run.py released --manifest $m --out $out
python recovery_run.py stalled --manifest $m --out $out
```

`revoked` uses X's actual existing app approval interface to revoke Y, then X
itself. It checks a fresh ordinary active-device directory and never revokes A
or Bea. `released` changes only the same gate's state and requires both release
events. `stalled` tries actual sends on both originals, requires bounded terminal
device-approval errors (including actual asynchronously failed outbox entries), verifies no new
server-accepted rows during each attempt, and requires unchanged pre-Add cursors.
A pending cache/outbox row, HTTP denial, or fabricated message ID cannot pass.
Keep actual client logs/previously authenticated roster evidence separately;
ordinary directory lists are not authenticated MLS rosters.

The refusal must exactly match the pinned app's untrusted-device error. Transport,
permission and other failures are inconclusive. Observations request the service's
maximum100-message page; a full page is inconclusive rather than evidence of
absence. Keep this fresh fixture below100 messages. Separately retain an actual
Y-authored current-group send received by X before revocation: Y reading an
X-authored message alone could be satisfied by sibling history fill.

**External checkpoint:** A opens the real conversation repair action and the
existing native verifier. Record the actual outcome. On Windows, an established
`NotConfigured`/`NotPresent` policy outcome must not be described as a successful
credential challenge. A canceled verification must publish no repair. After an
actual accepted repair, observe its immutable public chain:

```powershell
python recovery_run.py repair-observed --manifest $m --out $out
```

This independently verifies the immediate successor, canonical signature/hash
and unchanged independently pre-pinned external sender. It does not claim
receiver directory trust, prior membership, strict Welcome or engine acceptance.
Record those on the actual peers.

**External checkpoints:** Desktop GUI fresh sends and picture byte comparisons;
old text/picture reads; preserved X/Y actual fresh-ciphertext decryption refusal;
ordinary new sibling strict current Welcome; real Android native UI with
`hostId: local`, `deviceId: Pixel_7` and its bound device CLI; durable fixture-key
restart; chosen-marker server scan; recording captions/motion/duration validation.
The driver cannot mark any of these as proved. A host phone viewport and HTTP
access denial cannot substitute for native or cryptographic exclusion evidence.

## Retired-generation route probes

Supply `fences --captured $captures` only after the repair is observed. The
captures JSON has exactly `create`, `commit`, `tree`, `message`, `edit`,
`reaction`, `proposal`, each with the actual `{method,path,body}` from valid
owned generation0 requests. Retain capture provenance privately. Never use dummy
MLS payloads to infer cryptographic route validation. The driver replays each
with generation omitted and explicit0, requiring **409 generation_changed**.

The real encrypted edit route is POST conversation messages with `kind:edit`
and `target_id`; plaintext PATCH message is a different route. Ratchet-tree
upload is `ratchet_tree` inside POST commit. Proposal issuance is GET group-info.
Captured paths must stay within the owned conversation. Retain the separate
generation0 legacy-omission positive control, genuinely equal-epoch stale
captures, historical explicit0 handshake/tree reads and no-proposal observation.
Those additional controls remain explicit gaps in the route report.

No command retries a failed phase, discards an outbox message, deletes profiles,
extends a gate or overwrites old evidence. On failure retain the attempt for
diagnosis; the coordinator decides how to proceed in another fresh namespace.

Offline verification, with no live fixture or app:

```powershell
python -m unittest discover -s . -p 'test_*.py' -v
python recovery_run.py --help
```
