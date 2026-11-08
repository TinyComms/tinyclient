#!/usr/bin/env bash
# legacy-saved-speakers on Linux. The lab runs this in the console session it is already filming.
set -euo pipefail
cd "$(dirname "$0")"
exec python3 ./drive.py
