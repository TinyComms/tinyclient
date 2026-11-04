# The quality gate

Code does not reach people because a test passed. It reaches them because the real app was driven
on every platform it touches, and a recording of that run exists. This page is the whole gate:
what is checked, by whom, and what blocks.

Platforms: Windows, macOS, Linux, Android, iOS. Plus the server, which is its own repository
(`tinyserver`) and its own gate, written the same way.

## The rule

A `feature` or a `fix` merges only with an **e2e scenario** that:

1. is in the same pull request, so the proof moves with the change;
2. drives the real app (a release-profile build of this branch's SHA, not a mock) on every
   platform the diff reaches;
3. **fails on the base commit and passes on the branch**, both recorded, so the test is shown to
   test something;
4. leaves a **recording** of each platform's run that the lab itself made.

Nobody pastes their own screenshots as proof. The lab makes the recording and posts the status.

## Four gates

| Gate | When | Who runs it | What blocks |
| --- | --- | --- | --- |
| 1. Build and unit | every push, pull request | GitHub Actions (`ci.yml`, `mobile.yml`) | `ci` red |
| 2. Pull request | every pull request | `pr-gate.yml` (hosted) + the lab | `pr-gate` red, `e2e-proof` not green |
| 3. Release | before a cut | the lab, on the exact `origin/main` SHA | `release-cut.sh` BLOCK: no `e2e-release` |
| 4. Installed | after a release | the lab, on the installed build | not "done" (the `ship-it` skill) |

### Gate 2, in detail

`pr-gate` (hosted, cheap, fails in seconds) checks that the pull request says what it is:

- **template**: a description, and exactly one type.
- **scenario**: a `feature` or `fix` that changes code names an `e2e/scenarios/<id>` that exists
  on the branch **and is in the diff**.

It runs from the **base** commit, so a pull request that edits the gate is judged by the gate it
found, not the one it wrote.

The lab (see below) does the expensive part. It asks `pr-gate.py plan <number>` which platforms
the diff reaches, so the two cannot disagree. The path map is `e2e/gate.json`. A platform ticked
in the template adds to the plan; nothing takes one off. A change under `crates/` reaches all
five until the map is made finer from evidence.

For each platform in the plan the lab: builds the branch SHA, runs the scenario on the base (must
fail), runs it on the branch (must pass), records both, and posts `e2e-lab/<platform>`. When every
platform in the plan is green it posts **`e2e-proof`**. `refactor`, `perf` and `chore` need no new
scenario, but the lab still runs the existing smoke suite on their platforms and records it.
`docs` needs nothing and `pr-gate` fails a `docs` pull request that changes code.

### Gate 3, in detail

The release suite (`"release": true` in `scenario.json`) runs on the exact `origin/main` SHA the
captain is about to cut, on all five platforms, and the lab posts `e2e-release` on that SHA.
`packaging/release-cut.sh` prints it as `e2e:` and, once `e2e/gate.json` has `"release_proof":
"enforce"`, blocks without it. Everything else the captain checks is unchanged: one release a
local day, the freeze file, CI green on that SHA, a clean `main`.

### Gate 4, in detail

"Done" already means the installed app on Zeppelin reports the new version with screenshots. The
installed-build scenario (`smoke-installed`) turns that into a recording, including the update
from the previous release to this one through the real updater.

## The lab

The lab is the machines that already exist, driven over `ssh`. Nothing in GitHub runs on them.

| Platform | Host | Recorder | Checked 2026-11-04 |
| --- | --- | --- | --- |
| Linux | `tiny-linux` VM (X11, 1280x800) | ffmpeg `x11grab` | works; `vm-desktop.sh` already does it |
| iOS | `mbp` simulator (iPhone 18 Pro) | `xcrun simctl io <udid> recordVideo` | works: recorded h264 |
| macOS | `mbp` | ffmpeg `avfoundation` ("Capture screen 0") | device is there; Screen Recording permission for ffmpeg over ssh **not yet checked** |
| Android | Zeppelin AVD `Pixel_7` (WHPX) | `adb shell screenrecord`, chained (3 minute limit) | AVD exists; **not yet run** |
| Windows | `winbuild` VM (console session active) | ffmpeg `gdigrab` | session fine; **ffmpeg is not installed there** |

Why push and not a self-hosted runner: both repositories are public. A self-hosted runner on this
network runs whatever a fork pull request sends it. The lab only builds and runs a pull request
after an agent or Justin starts it, which is also how releases already work ("nothing tags or
releases with no agent in the thread").

How a run goes is in [`e2e/README.md`](../e2e/README.md).

## What blocks, and who can override it

Target branch protection on `main` (a settings change that needs Justin; nothing here applies it):

- required status checks: `test`, `package`, `pr-gate`, `e2e-proof`
- no force pushes, squash only (already true)
- **no admin bypass**, and the code-owner review requirement removed

The review requirement does nothing today. Justin is the only code owner and the author of every
pull request, so every merge is `--admin`. The gate replaces the review with something that can
actually say no. When it is on, `--admin` stops working for a pull request without `e2e-proof`,
and `ship-it` step 4 must stop using it.

Two honest limits. `e2e-proof` is a commit status, and anyone with write access can post one. The
lab posts it from one account and `pr-gate` reports the actor; a status from anyone else is a
finding, not a pass. And a recording proves the scenario ran, not that the scenario is good. A
reviewer still reads the scenario in the diff.

## Rolling it on

Each step is a line in `e2e/gate.json` or one branch-protection setting, so it can go on one
platform at a time and come off if it stalls the work.

1. **Now**: template, `pr-gate` with `template` and `scenario` enforced. Nothing needs a lab.
2. Linux + Android lab (the local call test already records Linux; add the Android recording).
   First scenario: the call, from `scripts/e2e/local-tiny-call.ps1`.
3. iOS (simulator, on the Mac) and macOS.
4. Windows (install ffmpeg on `winbuild`).
5. Require `e2e-proof` on `main`. Release suite on. `"release_proof": "enforce"`.

Before step 5, `ci` has to be reliable. The latest run on `main` is red only because
`test (macos-latest)` was cancelled; a required check that flakes stalls every pull request.
