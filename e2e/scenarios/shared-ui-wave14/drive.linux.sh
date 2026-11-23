#!/bin/bash
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
export TINYCLIENT_DIR=$(mktemp -d /tmp/tiny-wave14.XXXXXX)
export LC_ALL=C.UTF-8
CHATD=""
APP=""
cleanup() {
    [ -z "$APP" ] || kill "$APP" 2>/dev/null || true
    [ -z "$CHATD" ] || kill "$CHATD" 2>/dev/null || true
    rm -rf -- "$TINYCLIENT_DIR"
}
trap cleanup EXIT
"$E2E_APP_DIR/chatd" --seed-demo >"$E2E_OUT/chatd.log" 2>&1 &
CHATD=$!
"$E2E_APP_DIR/tinyclient" >"$E2E_OUT/app.log" 2>&1 &
APP=$!
/usr/bin/python3 "$HERE/drive-linux-atspi.py" "$APP"
