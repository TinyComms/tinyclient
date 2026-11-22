# Viewer sees a share

Use two dedicated lab OS users with the matching release build and isolated profiles, signed
into one disposable server with invented people A and B. Seed their direct conversation and
a video-enabled room named Proof. Leave both apps closed before the drivers start. The first
display on A must be the display with the test card. Install the usual lab input and capture
helpers and make Python with Tk available on A.

Run the platform driver on B with `E2E_CALL_VIDEO_PLAN` set to a private JSON file, or put the
plan in `~/.config/tiny-lab/call-video-<platform>.json` under the lab OS user. The plan
for each host has `app_dir`, `profile`, `out_dir` and `clicks`. The profile is the user's normal
tinyclient data directory, paired with their OS credential store containing just the fixture
session. Run each driver inside that user's interactive desktop session. Use window-relative clicks on
Windows and macOS, and screen coordinates on Linux. Provide `win_window` (the absolute path to
`scripts/e2e/win-window.ps1`), `x11input` (the path to `smoke/x11input.py`), or `mac_helper`
(the granted TinyLabHelper.app), as appropriate. On Linux `display_size` defaults to 1280x800.

- A needs `conversation`, `call`, `share`, `first_screen`, `stop_share`, `leave`, `room`
  and `join` coordinates for the corresponding visible controls.
- B needs `accept`, `leave`, `room`, `join` and `watch`. Its `video_rects` gives
  `direct` and `room` as four fractions `[left, top, width, height]` of the captured picture,
  covering the actual rendered share including the four corners but excluding letterbox bars.
  The picture is the desktop on Windows and Linux, and the app's window on macOS.
- B's `sender_command` is an argument array that starts A's platform driver with
  `--sender --plan <private-path>` in A's interactive GUI session. Its stdin/stdout carry
  coordination only. Start it through the lab session launcher appropriate to A's desktop.

Once the receiver's recorder starts, run B's driver. It opens both release windows, rings
from A, accepts on B, and drives the share picker on A. The full-screen card covers A's screen
until B has sampled its four colored quadrants and a changing number. A clicks Stop and
restores the test card while B checks that the picture stays gone. B explicitly clicks Watch
in the room half. The receiver
writes screenshots and timed steps from pixels only. The lab validator samples the recording
using the same checker.

Keep this scenario out of the release suite until it passes three consecutive lab runs on
Windows, Linux and macOS.
