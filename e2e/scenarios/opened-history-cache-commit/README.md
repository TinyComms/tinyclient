# Opened history before receive-state persistence

A failed sealed-cache append must not leave only a consumed receive ratchet. This
scenario drives the **normal Windows or Linux X11 app** (release-profile `tinyclient` + `chatd`
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
| `session_contract.py` | Literal-space `TINYCLIENT_SESSION_TINY SERVER` env |
| `ipc.py` | Fixture IPC via Windows `dlctl` or Linux profile Unix socket (not native proof) |
| `native_windows.py` / `owned_input.ps1` | Person-facing window input/pixels |
| `native_linux.py` | Owned executable/PID/foreground guards, X11 input and captured-image OCR |
| `drive.py` / platform launchers | Orchestrates the matched case |

`TINYCLIENT_PROVE` is refused. Diagnostic IPC may confirm decrypt/refuse; it does
not replace screenshots or window actions. Recordings follow `e2e/README.md`
(recorder before first step, playable mp4, ≥360px short side, SHA/step burn-in,
`steps.jsonl` / `proof.json`).

The conversation pixel oracle uses the Windows OCR API or Linux Tesseract word geometry.
Linux enlarges the captured image before OCR so small glyphs retain punctuation;
the exact marker match and conversation-pane boundary remain unchanged.
Missing OCR or a missing window is setup-incomplete. Both phases
must observe the exact server-assigned message ID in a scoped messages page;
an empty page while asynchronous backfill is running is never history-loss RED.
The peer is opened through the normal Ctrl+K Jump command, with literal query text
observed before Enter and foreground-PID guards. There are no guessed coordinates.
Message pixels are checked below the fresh-conversation intro in the opened pane;
a matching sidebar preview cannot pass that assertion.
Use a single-word synthetic display name per peer (for example Ada and Bea);
preflight rejects unsupported names before any profile is changed.

## Environment

- `E2E_OUT`, `E2E_T0`, `E2E_APP_DIR` — lab contract
- `E2E_CACHE_COMMIT_FIXTURE` — absolute JSON describing owned profiles/sessions,
  conversation id, and marker (`opened-history-cache-commit-…`)
- `E2E_DLCTL` — optional absolute `dlctl` when it is not beside the app bundle

Linux requires an X11 session with `DISPLAY` and its authentication environment, plus `wmctrl`,
`xdotool`, `ffmpeg` and `tesseract`. A missing dependency or Wayland session is setup-incomplete.
Before each input, the adapter checks the foreground window's PID and the owned executable.
Linux fixture IPC reads the same four-byte-length-prefixed JSON frames from the profile's
`chat.sock`; an explicit `E2E_DLCTL` override retains the helper route.

Each fixture session must include persistent `e2ee.state` and `e2ee.cache` wrapping
keys (base64, 32 bytes each), created once for a NEW fixture before its first app
launch. Missing/invalid keys refuse setup before any profile is changed. Keep
these synthetic keys private; never regenerate them to resume an existing device.

No real host/provider names, IPs, SIDs, tokens, or old user data belong in public
scripts or recordings. Fixture bootstrap stays env-driven and private.

## Scope

Windows and Linux X11 native adapters are available. This does not establish aggregate
`e2e-proof` / release status across platforms. Component daemon prove modes are
not the native evidence for this scenario. No production snapshot rewind, key
reset, or replay/attribution relaxation.
