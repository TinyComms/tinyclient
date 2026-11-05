# Device sync

Catch-up recovery harness preparation and its separate pending real-device proof
are documented in [RECOVERY.md](RECOVERY.md). The owned handshake gate and public
binding observer tests do not constitute a runtime recovery pass.

This is the reusable multi-device IPC proof. It drives the same chatd requests as the window,
records real desktop windows, and keeps one assertion ledger. It is **IPC proof with GUI display**;
it does not certify clicking through the link screens.

The Windows coordinator starts Ada A, Ada B and Bea in isolated profiles. A fourth app runs on
the `tiny-linux` SSH alias. A throwaway server and database contain invented accounts only.
The scenario checks encrypted DM and channel history, edits, reply references, actual downloaded
picture bytes and SHA-256, immediate incoming messages, outgoing identity, revocation, and
wrong-code, substituted-key, corrupt-archive, expired-code and reused-code failures.
Picture references come from matching downloads on both original devices before linking. Tiny
cleans image metadata before encryption, so raw input PNG bytes are retained as provenance but
are not the expected uploaded bytes. New devices, backup restore and gap fill must match the
pre-link reference hash and length exactly; image similarity is not sufficient.

The first post-link message is sent after one cache snapshot, before history pagination. Its
elapsed time from link completion is recorded. No commit wait is inserted before the send.

## Run on a Windows coordinator

Requirements: Rust, Python with Pillow and cryptography, PowerShell 7, Docker, ffmpeg/ffprobe,
the lab's existing desktop tools, and plain SSH access through the configured `tiny-linux` alias.
Supply the client and server commits explicitly. Neither repo is switched or built from dirty files.

1. Build the Windows app from a `git archive` export of the client commit, using
   `cargo build --release -p tinyclient --features gui -p chatd -p mediad -p login`.
   Keep `tinyclient-login[.exe]` beside chatd; its approval command must come from the same commit.
2. Build the IPC helper with `cargo build --release --manifest-path dlctl/Cargo.toml
   --target-dir "$env:LOCALAPPDATA/tiny-e2e/dlctl-target"`.
3. In Git Bash, run `bash drivers/prepare-linux.sh <client-repo> release <client-sha>`.
   It prints the immutable `~/tiny-dl/bin/apps/<client-sha>` path for `--linux-app-dir`.
4. Set `DL_ADVERTISE` to the coordinator endpoint from private lab configuration when remote
   devices need it. Supply a literal address; the local default is `127.0.0.1`.
   No private endpoint is committed here.
5. Run:

```powershell
python dl.py run --app-dir <pinned-windows-binaries> --server-dir <server-repo> `
  --client-sha <client-sha> --linux-app-dir "~/tiny-dl/bin/apps/<client-sha>" `
  --linux-state-dir "/tmp/tiny-dl-<unique-run-id>" `
  --server-ref <server-sha> --devices desk_b,linux --history 6 --link-ttl 60 `
  --cases reject,mitm,tamper,reuse,expired --extended --out <fresh-evidence-directory>
```

Exit zero means every decided assertion passed. `summary.json` records every failure and skip.
Stop the throwaway fixture with `pwsh -File server/dl-server.ps1 down` after preserving evidence.
The paired base run uses the shipped app with `--mode legacy` and its own fresh evidence folder.
A base failure remains a nonzero process exit and is identified by `base-as-expected` in the ledger.

Each database container carries scenario and state-directory ownership labels. The fixture refuses
to reset or remove a container with other labels. Use `--pg-container`, `--pg-port`, `--http-port`,
`--proxy-port`, `--oidc-port`, `--media-port`, and `DL_STATE` for a separate concurrent fixture; pass the same
container and state parameters to `dl-server.ps1 down`. Never use another test run's container.
`--client-sha` checks the Windows build's `SOURCE_SHA` file and Linux build's source marker, keeps
binary SHA-256 values, and burns the client/server SHAs into the desktop recording captions.

## Lab integration boundary

The descriptor travels with feature changes so the PR gate can locate the scenario. The lab
dispatches `execution: coordinated-ipc` to `e2e/lab/coordinated.py`, before any single-app adapter
builds or touches a device. Windows and Linux workers share one fresh coordinator invocation per
client commit and base/branch label. Each platform still receives its own raw recording and steps
through the lab's independent motion, duration and caption checks. A scratch run or a passing
assertion ledger alone cannot supply a lab pass.

Android, iOS and macOS need native coordinator participants and currently fail this boundary;
their demo smoke adapters cannot certify this scenario. macOS and iOS remain deferred. Keep
`release` false until the whole required platform plan is recorded. Add `--extended` to run backup export, wrong-passphrase and
tampered-file rejection, a nonempty import roundtrip, and same-user gap fill after linking with
no archive history. Keep `release` false until the whole required platform plan is recorded.
The extended sync case imports complete history, injects one transient self-conversation handshake
503 for that fresh device, and checks that
an old device's new verified mark reaches it before it sends any local Want or Verified message.
It uses a separate profile from the no-history gap-fill test so missing history cannot initialize
sync first and conceal a readiness bug.
The fresh profiles in these extended cases run chatd without a window so shell background reads
cannot initialize sibling sync or fill history before the assertion. These are IPC checks.

The device-sync feature stack requires `--extended`: its evidence must include a nonempty
backup import and sibling gap fill, as well as the initial linked-device send. The descriptor
records those requirements; it remains a coordinated IPC scenario pending native lab adapters.

The canary scan reports only whether its chosen needles were found in the chosen files; it is
not a security audit. Seed sessions and link-code inputs are local test secrets and must not be
published with public evidence.

Run harness checks with `python -m unittest discover -s . -p 'test_*.py' -v`.

## Private lab configuration

Set `E2E_DEVICE_SYNC_CONFIG` to a private JSON file outside the repository. It contains
`server_dir`, `server_sha` (full committed SHA), `app_dirs` (full client SHA to immutable Windows
binary folder), `linux_app_dirs` (the same SHA to immutable folder on `tiny-linux`),
`timeout_seconds` (1..14400), `advertise` (the coordinator's IP from private configuration), and
`ports` with five distinct explicit integers named `pg`, `http`, `proxy`, `oidc`, and `media`.
The address must be reachable from the remote participant. Both base and branch need pinned
binary entries before a PR run. Windows folders contain `SOURCE_SHA`; Linux folders contain
`SHA`. Source markers and native binary digests are retained; an older build is never relabeled.

The lab exports the exact app source archive for its helpers, overlays the branch scenario,
and supplies `E2E_SCENARIO_SHA`. It runs all negative cases and `--extended`, with a unique
database container, local `DL_STATE`, and short Linux `/tmp` profile namespace. It never reuses
an existing fixture. The shared Linux helper must be prepared from this scenario's committed
driver version before running; preparation is a separate deliberate action, not an implicit
overwrite of a helper another proof might be using. The IPC helper at the documented local
`dlctl-target/release` location must already be built against the matching protocol.

From the client repository, after preparing pinned builds and choosing unused private ports:

```powershell
$env:E2E_DEVICE_SYNC_CONFIG = '<private-config-file>'
python e2e/lab/lab.py try device-sync --sha <full-client-sha> --platforms windows,linux
```

This runs the real coordinator and records both desktops; `try` posts and uploads nothing. A PR
run additionally needs the paired base result to be `base-as-expected`. Infrastructure failures,
missing recordings, mismatched provenance and cleanup failures cannot count as that regression.
Existing motion thresholds are unchanged.

The scenario emits `lab-result.json` with `schema: 1`, `execution: coordinated-ipc`, client/server
SHAs, `completed`, and per-platform `recording`, `steps`, `seconds`, `binary_sha256`, and `vfr`.
Paths are relative to the fresh evidence root and must remain inside their own platform folder.
`seconds` comes from probing the raw recording; the consumer still probes and checks the video
itself. This manifest supplies evidence locations, never an exemption from recording validation.
The private assertion summary remains local because it contains fixture inputs and local paths.

On completion, interruption or deadline, the lab calls `dl.py cleanup --state <owned-state>` and
the labeled fixture's `down` action. The device registry checks native Windows process creation
time and executable; Linux stop checks its native PID/start-time/executable/UID ownership record.
Identity mismatches are retained and reported without stopping that process. Cleanup errors fail
the platform result. The deadline stops the exact coordinator process and the registered app,
recorder, fixture and remote participant processes. It does not guarantee termination of an
unregistered build or video-conversion descendant. A timeout remains failed evidence and may
require inspecting those owned jobs before another run; no process-name sweep is performed.
Native mobile participation must provide the same isolated lifecycle,
production core, exact build provenance, raw platform recording and assertion ledger before it
can be registered; substituting desktop evidence or demo data is not a supported integration.
