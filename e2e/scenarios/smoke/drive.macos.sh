#!/bin/bash
# smoke on macOS. Runs ON the Mac (the lab ships it with the scenario), under e2e/lab/platforms/macos.py.
#
# The app is the real tinyclient.app build, started against a scratch TINYCLIENT_DIR that
# `chatd --seed-demo=live` filled with invented people. What the driver asserts comes from the
# app's own state: `tinyclient --text` asks the same chatd the window is showing. Keys go to the
# app's process only (TinyLabHelper), never to whatever window has focus.
#
# env from the lab: E2E_T0 (epoch seconds of the recorder's start), E2E_OUT, E2E_APP_DIR (the .app),
# E2E_HELPER (TinyLabHelper.app); E2E_SABOTAGE=1 seeds nothing, so the inbox step must fail.
set -u
: "${E2E_T0:?}" "${E2E_OUT:?}" "${E2E_APP_DIR:?}" "${E2E_HELPER:?}"
BIN=$E2E_APP_DIR/Contents/MacOS
STEPS=$E2E_OUT/steps.jsonl
: > "$STEPS"
FAILED=0
DATA=$(mktemp -d /tmp/tl.XXXXXX)   # short: a unix socket path may not pass 104 bytes
CHATD="" APP=""

step() {  # step "words" true|false
    local t
    t=$(perl -MTime::HiRes=time -e 'printf "%.2f", time - $ARGV[0]' "$E2E_T0")
    printf '{"t": %s, "step": "%s", "ok": %s}\n' "$t" "$1" "$2" >> "$STEPS"
    [ "$2" = true ] || FAILED=1
}
helper() {
    local f; f=$(mktemp "$E2E_OUT/h.XXXXXX")
    open -W -n --stdout "$f" --stderr "$f" -a "$E2E_HELPER" --args "$@" >/dev/null 2>&1
    cat "$f"; rm -f "$f"
}
text() { TINYCLIENT_DIR=$DATA "$BIN/tinyclient" --text 2>/dev/null; }
# Only processes this driver started, by pid.
cleanup() {
    [ -n "$APP" ] && kill -0 "$APP" 2>/dev/null && kill -TERM "$APP" 2>/dev/null
    [ -n "$CHATD" ] && kill -0 "$CHATD" 2>/dev/null && kill -TERM "$CHATD" 2>/dev/null
    sleep 0.5
    [ -n "$APP" ] && kill -0 "$APP" 2>/dev/null && kill -KILL "$APP" 2>/dev/null
    [ -n "$CHATD" ] && kill -0 "$CHATD" 2>/dev/null && kill -KILL "$CHATD" 2>/dev/null
    rm -rf "$DATA"
}
trap cleanup EXIT

# 1. Invented data. Sabotage leaves chatd empty, which is what a broken seed looks like.
if [ "${E2E_SABOTAGE:-0}" = 1 ]; then SEED=""; else SEED="--seed-demo=live"; fi
TINYCLIENT_DIR=$DATA "$BIN/chatd" $SEED >"$E2E_OUT/chatd.log" 2>&1 &
CHATD=$!
READY=false
for _ in $(seq 1 100); do
    if text >"$E2E_OUT/inbox.txt"; then READY=true; break; fi
    sleep 0.2
done
if $READY && [ "${E2E_SABOTAGE:-0}" != 1 ]; then ROWS=$(grep -c . "$E2E_OUT/inbox.txt"); [ "$ROWS" -gt 20 ] || READY=false; fi
step "seeded demo data ready" $READY

# 2. The window. Its pid goes to the recorder, which films that process's window and nothing else.
TINYCLIENT_DIR=$DATA "$BIN/tinyclient" >"$E2E_OUT/app.log" 2>&1 &
APP=$!
echo "$APP" > "$E2E_OUT/app.pid"
SHOWN=false
for _ in $(seq 1 100); do
    kill -0 "$APP" 2>/dev/null || break
    if helper windows --pid "$APP" | grep -q '"id"'; then SHOWN=true; break; fi
    sleep 0.2
done
step "app launched and its window is on screen" $SHOWN
$SHOWN && sleep 2.5   # first frame of the list

# 3. The inbox, from the app's own side: the same chatd answers `tinyclient --text`.
text >"$E2E_OUT/inbox.txt"
if grep -q "Foray carpool" "$E2E_OUT/inbox.txt" && grep -q "Ines Marchetti" "$E2E_OUT/inbox.txt" && grep -q "Climbing Sundays" "$E2E_OUT/inbox.txt"; then
    step "inbox lists the demo conversations" true
else
    step "inbox lists the demo conversations" false
fi

# 4. Open the first conversation: option-down is the list's "next row", which opens it. Opening
#    it reads it, so its unread badge clears in chatd.
UNREAD_BEFORE=$(grep -E "^\S+\s+showcase\s+[0-9]" "$E2E_OUT/inbox.txt" | head -1)
OUT=$(helper key --pid "$APP" --combo alt-down)
OPENED=false
if echo "$OUT" | grep -q '^ok'; then
    for _ in $(seq 1 30); do
        sleep 0.3
        text >"$E2E_OUT/inbox-after.txt"
        if [ -n "$UNREAD_BEFORE" ] && ! grep -E "^\S+\s+showcase\s+[0-9]" "$E2E_OUT/inbox-after.txt" >/dev/null; then OPENED=true; break; fi
    done
else
    echo "key: $OUT" >> "$E2E_OUT/driver-notes.txt"
fi
step "first conversation opens and is read" $OPENED
sleep 2.5   # the conversation on screen

# 5. Quit the way a person does: command-Q, and the process must exit by itself with status 0.
CLEAN=false
if kill -0 "$APP" 2>/dev/null; then
    helper key --pid "$APP" --combo cmd-q >/dev/null
    for _ in $(seq 1 40); do kill -0 "$APP" 2>/dev/null || break; sleep 0.25; done
    if ! kill -0 "$APP" 2>/dev/null; then wait "$APP"; [ $? -eq 0 ] && CLEAN=true; fi
fi
APP=""
step "app quits cleanly" $CLEAN

[ "$FAILED" = 0 ]
