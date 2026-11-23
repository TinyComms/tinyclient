#!/bin/bash
# discord-wave17b on the iOS simulator host. The prove is chatd's, same as the desktop driver.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
if command -v python3 >/dev/null 2>&1; then
    exec python3 "$HERE/drive.py"
fi
echo "python3 is not on PATH" >&2
exit 1
