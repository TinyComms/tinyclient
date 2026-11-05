# Device sync

This is the reusable multi-device IPC proof. It drives the same chatd requests as the window,
records real desktop windows, and keeps one assertion ledger. It is **IPC proof with GUI display**;
it does not certify clicking through the link screens.

The Windows coordinator starts Ada A, Ada B and Bea in isolated profiles. A fourth app runs on
the `tiny-linux` SSH alias. A throwaway server and database contain invented accounts only.
The scenario checks encrypted DM and channel history, edits, reply references, actual downloaded
picture bytes and SHA-256, immediate incoming messages, outgoing identity, revocation, and
wrong-code, substituted-key, corrupt-archive, expired-code and reused-code failures.

The first post-link message is sent after one cache snapshot, before history pagination. Its
elapsed time from link completion is recorded. No commit wait is inserted before the send.

## Run on a Windows coordinator

Requirements: Rust, Python with Pillow and cryptography, PowerShell 7, Docker, ffmpeg/ffprobe,
the lab's existing desktop tools, and plain SSH access through the configured `tiny-linux` alias.
Supply the client and server commits explicitly. Neither repo is switched or built from dirty files.

1. Build the Windows app from a `git archive` export of the client commit, using
   `cargo build --release -p tinyclient --features gui -p chatd -p mediad`.
2. Build the IPC helper with `cargo build --release --manifest-path dlctl/Cargo.toml
   --target-dir "$env:LOCALAPPDATA/tiny-e2e/dlctl-target"`.
3. In Git Bash, run `bash drivers/prepare-linux.sh <client-repo> release <client-sha>`.
4. Set `DL_ADVERTISE` to the coordinator endpoint from private lab configuration when remote
   devices need it. The default is `localhost`; no private endpoint is committed here.
5. Run:

```powershell
python dl.py run --app-dir <pinned-windows-binaries> --server-dir <server-repo> `
  --server-ref <server-sha> --devices desk_b,linux --history 6 --link-ttl 60 `
  --cases reject,mitm,tamper,reuse,expired --extended --out <fresh-evidence-directory>
```

Exit zero means every decided assertion passed. `summary.json` records every failure and skip.
Stop the throwaway fixture with `pwsh -File server/dl-server.ps1 down` after preserving evidence.
The paired base run uses the shipped app with `--mode legacy` and its own fresh evidence folder.
A base failure remains a nonzero process exit and is identified by `base-as-expected` in the ledger.

Each database container carries scenario and state-directory ownership labels. The fixture refuses
to reset or remove a container with other labels. Use `--pg-container`, `--pg-port`, `--http-port`,
`--proxy-port`, `--oidc-port`, and `DL_STATE` for a separate concurrent fixture; pass the same
container and state parameters to `dl-server.ps1 down`. Never use another test run's container.
`--client-sha` checks the Windows build's `SOURCE_SHA` file and Linux build's source marker, keeps
binary SHA-256 values, and burns the client/server SHAs into the desktop recording captions.

## Lab integration boundary

The descriptor travels with feature changes so the PR gate can locate the scenario. The current
one-app platform adapters do not yet stand up `device-sync-coordinator` or `server-device-link`.
They must fail until those fixtures and native drivers exist. This scenario therefore does not
post `e2e-proof`, claim all-platform coverage, or convert a standalone scratch run into a lab pass.
Android, iOS and macOS are gaps. Add `--extended` to run backup export, wrong-passphrase and
tampered-file rejection, a nonempty import roundtrip, and same-user gap fill after linking with
no archive history. Keep `release` false until the whole required platform plan is recorded.
The extended sync case injects one transient handshake 503 for the fresh device and checks that
an old device's new verified mark reaches it before it sends any local Want or Verified message.
The fresh profiles in these extended cases run chatd without a window so shell background reads
cannot initialize sibling sync or fill history before the assertion. These are IPC checks.

The device-sync feature stack requires `--extended`: its evidence must include a nonempty
backup import and sibling gap fill, as well as the initial linked-device send. The descriptor
records those requirements; it remains a coordinated IPC scenario pending native lab adapters.

The canary scan reports only whether its chosen needles were found in the chosen files; it is
not a security audit. Seed sessions and link-code inputs are local test secrets and must not be
published with public evidence.

Run harness checks with `python -m unittest discover -s . -p 'test_*.py' -v`.
