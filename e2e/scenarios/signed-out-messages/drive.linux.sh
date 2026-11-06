#!/usr/bin/env bash
# signed-out-messages on Linux. The lab runs this with bash while ffmpeg films the display.
set -euo pipefail
python3 "$(dirname "$0")/drive.py"