#!/bin/bash
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
export TINYCLIENT_DIR=$(mktemp -d /tmp/tiny-wave14.XXXXXX)
BIN="$E2E_APP_DIR/Contents/MacOS"
CHATD=""
APP=""
cleanup() {
    [ -z "$APP" ] || kill "$APP" 2>/dev/null || true
    [ -z "$CHATD" ] || kill "$CHATD" 2>/dev/null || true
    rm -rf -- "$TINYCLIENT_DIR"
}
trap cleanup EXIT
xcrun swiftc "$HERE/drive-macos-ax.swift" -o "$E2E_OUT/drive-macos-ax"
"$BIN/chatd" --seed-demo >"$E2E_OUT/chatd.log" 2>&1 &
CHATD=$!
LC_ALL=en_US.UTF-8 "$BIN/tinyclient" >"$E2E_OUT/app.log" 2>&1 &
APP=$!
printf '%s\n' "$APP" > "$E2E_OUT/app.pid"
"$E2E_OUT/drive-macos-ax" "$APP"
