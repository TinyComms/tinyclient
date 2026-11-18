#!/bin/bash
# quick-actions on macOS. Runs ON the Mac (the lab ships it with the scenario), under
# e2e/lab/platforms/macos.py.
#
# On macOS the quick actions are the Dock menu gpui builds from the app's own registration;
# clicking it is the system's own UI and needs the Screen Recording and Accessibility grants the
# lab's Mac does not have yet (e2e/README.md). What this driver can prove from the process side,
# without either permission, is the argument path the menu items stand for: a launch carrying
# `--action quit` ends by itself with status 0 once the window is up. On a build without the
# actions the argument is ignored, the window stays open and the process keeps running, so the
# step fails.
#
# env from the lab: E2E_T0 (epoch seconds of the recorder's start), E2E_OUT, E2E_APP_DIR (the
# .app), E2E_HELPER (TinyLabHelper.app).
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

# 1. Invented data, so the window that opens is the signed-in one a person quick-actions into.
TINYCLIENT_DIR=$DATA "$BIN/chatd" --seed-demo=live >"$E2E_OUT/chatd.log" 2>&1 &
CHATD=$!
READY=false
for _ in $(seq 1 100); do
    if text >"$E2E_OUT/inbox.txt"; then READY=true; break; fi
    sleep 0.2
done
step "seeded demo data ready" $READY

# 2. A launch carrying New Message opens the window as usual (on the Mac the Dock menu item is
#    the same action in the running app; the argument is its launch form).
TINYCLIENT_DIR=$DATA "$BIN/tinyclient" --action new-message >"$E2E_OUT/app.log" 2>&1 &
APP=$!
echo "$APP" > "$E2E_OUT/app.pid"
SHOWN=false
for _ in $(seq 1 100); do
    kill -0 "$APP" 2>/dev/null || break
    if helper windows --pid "$APP" | grep -q '"id"'; then SHOWN=true; break; fi
    sleep 0.2
done
step "a launch carrying New Message opens the window" $SHOWN

# 3. A launch carrying Quit ends by itself with status 0: the running window gets the action,
#    quits, and is gone within 20 s. Without the actions this process keeps running.
TINYCLIENT_DIR=$DATA "$BIN/tinyclient" --action quit >"$E2E_OUT/quit.log" 2>&1 &
QUIT=$!
ENDED=false
for _ in $(seq 1 80); do
    kill -0 "$QUIT" 2>/dev/null || { ENDED=true; break; }
    sleep 0.25
done
RC=""
$ENDED && { wait "$QUIT"; RC=$?; }
OK=false
$ENDED && [ "$RC" = 0 ] && OK=true
step "a launch carrying Quit ends the app by itself (exit ${RC:-none})" $OK

# 4. The first window is closed the way a person closes it.
CLEAN=false
if kill -0 "$APP" 2>/dev/null; then
    helper key --pid "$APP" --combo cmd-q >/dev/null
    for _ in $(seq 1 40); do kill -0 "$APP" 2>/dev/null || break; sleep 0.25; done
    if ! kill -0 "$APP" 2>/dev/null; then wait "$APP"; [ $? -eq 0 ] && CLEAN=true; fi
fi
APP=""
step "the window quits cleanly" $CLEAN

[ "$FAILED" = 0 ]
