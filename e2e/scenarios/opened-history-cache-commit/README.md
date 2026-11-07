# Opened history before receive-state persistence

A failed sealed-cache append must not leave only a consumed receive ratchet. This
scenario drives the **normal Windows app** (release-profile `tinyclient` + `chatd`
+ `mediad`) against an owned disposable Tiny backend and synthetic people.

## Matched case

1. Prepare a valid encrypted conversation and receiver device with no prior target
   cache log. Wrong login, missing group, or failed setup is not a valid RED.
2. Block only the target sealed-cache append with an empty directory at its
   otherwise absent `cache-<conversation>.log` path. Parent/state store stays
   writable. An owned peer delivers a unique encrypted marker.
3. Under the fault, the base must actually decrypt (so later loss is not missing
   keys). The fixed build must refuse a durable open (a temporary stub is not
   success).
4. Cold-stop owned processes, remove only the blocker, empty only this fixture's
   SQLite message index, keep conversation/MLS/keys, and restart.
5. Shared assertion: `opened plaintext is still present after cache-fault repair
   and cold restart` — fails on the base for that history-loss reason, passes on
   the branch.

## Layout

| File | Role |
| --- | --- |
| `fixture_controller.py` | Synthetic fixture paths, empty-dir blocker, SQLite wipe |
| `session_contract.py` | Literal-space `TINYCLIENT_SESSION_TINYSERVER` env |
| `ipc.py` | Fixture/diagnostic chatd IPC via `dlctl` (not native proof) |
| `native_windows.py` / `owned_input.ps1` | Person-facing window input/pixels |
| `drive.py` / `drive.windows.ps1` | Orchestrates the matched case |

`TINYCLIENT_PROVE` is refused. Diagnostic IPC may confirm decrypt/refuse; it does
not replace screenshots or window actions. Recordings follow `e2e/README.md`
(recorder before first step, playable mp4, ≥360px short side, SHA/step burn-in,
`steps.jsonl` / `proof.json`).

## Environment

- `E2E_OUT`, `E2E_T0`, `E2E_APP_DIR` — lab contract
- `E2E_CACHE_COMMIT_FIXTURE` — absolute JSON describing owned profiles/sessions,
  conversation id, and marker (`opened-history-cache-commit-…`)
- `E2E_DLCTL` — optional absolute `dlctl` when it is not beside the app bundle

No real host/provider names, IPs, SIDs, tokens, or old user data belong in public
scripts or recordings. Fixture bootstrap stays env-driven and private.

## Scope

Windows person-facing proof only. This does not establish aggregate
`e2e-proof` / release status across platforms. Component daemon prove modes are
not the native evidence for this scenario. No production snapshot rewind, key
reset, or replay/attribution relaxation.
