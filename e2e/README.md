# e2e

Scenarios that drive the real app, and the contract the lab follows to record them. The reasons
are in [`docs/quality-gate.md`](../docs/quality-gate.md). This page is what you need to add a
scenario or build a lab host.

## A scenario

```
e2e/scenarios/<id>/
  scenario.json      what it proves, where it runs, what it needs
  drive.<platform>   one driver per platform: exits 0 on pass, 1 on a failed step
```

`<id>` is lowercase letters, digits, `.`, `_`, `-`.

```json
{
  "id": "mute-room",
  "title": "A member mutes a room from the sidebar menu and stops getting its sounds",
  "platforms": ["windows", "macos", "linux"],
  "needs": ["server", "two-people"],
  "release": false
}
```

- `platforms`: where it must pass. The lab runs at least the platforms the diff reaches that are
  listed here. A platform the scenario does not list is a gap the reviewer asks about.
- `needs`: what the lab stands up first. `server` is a `tinyserver` built from `origin/main`
  with a fresh database and the seed (`scripts/e2e/local-tiny-call.ps1 -Phase server`).
  `two-people` is a second signed-in person on another host.
- `release`: `true` puts it in the release suite (gate 3). Keep that suite small. Every scenario
  in it runs on five platforms before every cut.

A driver talks to the app the way a person does and nothing else. Desktop: window input and
pixels (`scripts/e2e/win-window.ps1`, `TINYCLIENT_DEBUG_JOIN`). Phones: the debug launch extras
and the platform's own input. It writes `steps.jsonl` beside it, one line per step:

```json
{"t": 12.4, "step": "open the room menu", "ok": true, "shot": "03-menu.png"}
```

Test data is invented. Drivers run with an isolated `TINYCLIENT_DIR` and made-up people, so a
recording never shows a real account, and the recordings are public.

## A lab run

For one pull request on one platform, the lab host does this and nothing else:

1. Build the branch SHA, and the base SHA, with the release profile and the features the shipped
   app has. A build that differs from the shipped one proves nothing.
2. Stand up what `needs` names.
3. Start the recorder. **Then** run the base build's driver. It has to fail. Stop the recorder.
   Keep the recording.
4. The same for the branch build. It has to pass.
5. Check each recording (below). Upload both with `steps.jsonl`, the logs and `proof.json`.
6. Post `e2e-lab/<platform>` on the pull request's head SHA: `success` only when the base run
   failed, the branch run passed, and both recordings are valid. `target_url` is the run's page.

When every platform in `pr-gate.py plan <number>` has a success, post `e2e-proof`. A scenario
that passes on the base proves nothing new, so the lab reports `failure` and says that.

A refactor, perf change or chore has no new scenario: step 3 is skipped, and the lab runs the
existing `smoke` scenarios on the branch only.

### A valid recording

The lab checks these with `ffprobe` and `ffmpeg`, and fails the platform if any is false.

- Started before the first step and stopped after the last, so no step happens off camera.
- Playable, with a short side of at least 360 px, and no step later than the end of the run: no step
  happens off camera. A recorder that writes frames only when the screen changes (`screenrecord`,
  `simctl`) ends its file at the last change; the adapter says `vfr=True` and the lab pads the clip.
- Not black, and it moves: at least a second of change in the run, and no single still stretch longer
  than the scenario's `max_still_seconds` (default 20). A screen holding still is normal for a UI.
  A run that was meant to fail (the base) is checked only for playing and showing its steps: a
  failing app may sit still or go dark, and that is the evidence.
- The SHA and the step name are burned into the picture (`drawtext` from `steps.jsonl`), so a
  clip cannot be moved to another pull request.
- Audio scenarios also keep the received audio and run `scripts/e2e/tone-check.py` on it.

### `proof.json`

```json
{
  "repo": "TinyComms/tinyclient", "pr": 170, "sha": "…", "base": "…",
  "platform": "ios", "host": "mbp", "scenario": "mute-room",
  "base_run":   {"passed": false, "recording": "ios-base.mp4",   "sha256": "…", "seconds": 41.2},
  "branch_run": {"passed": true,  "recording": "ios-branch.mp4", "sha256": "…", "seconds": 44.0},
  "binary_sha256": "…", "started": "2026-11-04T15:20:00Z"
}
```

### Where files go

A release of `TinyComms/e2e-proof` (a repository that holds only recordings), tagged
`pr-<number>-<sha7>` for a pull request and `main-<sha7>` for the release suite. Assets hold up
to 2 GB each and cost nothing. The `target_url` of each status is that release.

## Recorders, per host

| Platform | Host | Start | Stop |
| --- | --- | --- | --- |
| Linux | `tiny-linux` | `ffmpeg -f x11grab -framerate 10 -video_size 1280x800 -i :0 -c:v libx264 -preset ultrafast -pix_fmt yuv420p out.mp4` | `kill -INT` (so the file is finished) |
| iOS | `mbp` | `xcrun simctl io <udid> recordVideo --codec h264 out.mp4` | `kill -INT` |
| macOS | `mbp` | `ffmpeg -f avfoundation -framerate 10 -i "Capture screen 0" …` | `kill -INT` |
| Android | the emulator | `adb shell screenrecord --time-limit 180 /sdcard/n.mp4`, one after another, joined with `ffmpeg -f concat` | until the run ends |
| Windows | `winbuild` | `ffmpeg -f gdigrab -framerate 10 -i desktop …` in the console session | `q` on stdin |

macOS is recorded as the tinyclient window only, never the whole display: the Mac is a laptop in
daily use and the recordings are public.

## Not built yet

Built: `pr-gate`, the pull request template, `gate.json`, the `e2e:` line in `release-cut.sh`, and
the lab (`e2e/lab/lab.py`, `common.py`, `platforms/`), with one scenario, `smoke`.

```
python e2e/lab/lab.py try smoke --platforms linux     # branch build only, posts nothing
python e2e/lab/lab.py pr 170                          # base then branch, posts the statuses
python e2e/lab/lab.py release                         # the release suite on origin/main
python -m unittest discover -s e2e/lab -p "test_*.py"  # needs ffmpeg and ffprobe on PATH
```

`try` reads the scenario from the working tree; `pr` and `release` read it from the commit. The
lab needs `gh` signed in as the account in `gate.json` `lab_actors`, `ssh` to the hosts, and
ffmpeg/ffprobe here (`E2E_FFMPEG`).

### Status of each platform (2026-11-04)

| Platform | State | Evidence |
| --- | --- | --- |
| Linux | works | `lab.py try smoke`: validated, captioned recording; sabotage run fails on the inbox step; warm run 22 s |
| Windows | works | the same on `winbuild`; ffmpeg 9.0.2 and protoc 29.3 installed in `C:\lab\tools` (no admin) |
| Android | works | the same on a headless `Pixel_7`; builds the `demo` APK |
| macOS | **blocked** | needs Screen Recording and Accessibility for `~/Development/tiny-lab/bin/TinyLabHelper.app` (System Settings > Privacy & Security). Not granted, so the recorder, key presses and driver are untested. |
| iOS | **blocked** | build, install and the conversation tap work; no passing `try` run yet. The first inbox check failed on a white screenshot while the recording showed the inbox, cause not found. |
| Mac host | **blocked** | `~/Development` is on an external drive that stalled (`ls` fails). The internal disk is at 97%, so the build cannot move there without a decision. |

Nothing in this table is a pass for macOS or iOS. Until both have a recording, `release_proof` stays
`report` and `e2e-proof` is not made a required check, or every change under `crates/` would be
unmergeable.

Known departures from the shipped app: the Android and iOS clips are a `demo` build (invented data
compiled in), and at `origin/main` 957aaae the Android `demo` build does not compile
(`mobile/android/loopback.rs` uses the old `mediad::picture::phone::Pictures`), so the adapter drops
those two lines. `smoke` never opens the call screen, which is the only place they are used.
