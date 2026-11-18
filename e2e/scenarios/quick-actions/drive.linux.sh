#!/bin/bash
# quick-actions on Linux (X11), run ON the host by e2e/lab/platforms/linux.py while ffmpeg films :0.
#
# A launcher's quick action is an argument a second launch carries (`--action <name>`), handed to
# the running window over the single-instance handoff the way an invite link is; the second
# process exits. New Message opens the "to whom" switcher (the picture changes), Inbox is handed
# over, and Quit ends the app. On a build without the actions the second launch ignores the
# argument and opens its own window, so it never exits and the steps fail.
#
# The packaged .desktop file is not on this host: its Actions block is validated by the tinyclient
# tests (the desktop-file test in crates/tinyclient/src/quick.rs) and by desktop-file-validate in
# the packaging job. This driver proves what the arguments do to the running app.
#
# In:  E2E_OUT (steps.jsonl, shots and logs), E2E_T0, E2E_APP_DIR (tinyclient, chatd, mediad).
# Out: exit 0 only if every step is ok.
#
# All data is invented: chatd --seed-demo fills a scratch TINYCLIENT_DIR, and with TINYCLIENT_DIR
# set chatd never reads or writes the real store or credentials.
set -u
APP=${E2E_APP_DIR:?E2E_APP_DIR}
OUT=${E2E_OUT:?E2E_OUT}
T0=${E2E_T0:?E2E_T0}
mkdir -p "$OUT/shots"
: > "$OUT/steps.jsonl"

SCRATCH=$(mktemp -d /tmp/tiny-e2e-XXXXXX)      # short: chatd's socket path has a 108-byte limit
export TINYCLIENT_DIR=$SCRATCH/data
export XDG_CONFIG_HOME=$SCRATCH/xdg-config XDG_DATA_HOME=$SCRATCH/xdg-data XDG_STATE_HOME=$SCRATCH/xdg-state
export TINYCLIENT_LOG_LEVEL=debug RUST_LOG=info
mkdir -p "$TINYCLIENT_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"

FAILED=0
CHATD_PID=
GUI_PID=
SHOTN=0

at() { awk -v a="$(date +%s.%N)" -v b="$T0" 'BEGIN{printf "%.2f", a-b}'; }
shot() {  # a still of the screen (the same display the recorder films), named for the step
    local f=$1
    ffmpeg -nostdin -loglevel error -y -f x11grab -video_size 1280x800 -i "$DISPLAY" -frames:v 1 "$OUT/shots/$f" 2>/dev/null
}
step() {  # step true|false "words" [shot-name]
    local ok=$1 name=$2 f=""
    if [ -n "${3:-}" ]; then
        SHOTN=$((SHOTN + 1))
        f=$(printf '%02d-%s.png' "$SHOTN" "$3")
        shot "$f"
    fi
    [ "$ok" = true ] || FAILED=1
    if [ -n "$f" ]; then
        printf '{"t": %s, "step": "%s", "ok": %s, "shot": "shots/%s"}\n' "$(at)" "$name" "$ok" "$f" >> "$OUT/steps.jsonl"
    else
        printf '{"t": %s, "step": "%s", "ok": %s}\n' "$(at)" "$name" "$ok" >> "$OUT/steps.jsonl"
    fi
    echo "step: $ok: $name"
}
alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }
text() { "$APP/tinyclient" --text 2>/dev/null; }
rows() {  # the inbox lines of `tinyclient --text`: everything after the "media" line, minus "call"
    text | sed -n '/^media /,$p' | tail -n +2 | grep -v '^call '
}
wait_for() {  # wait_for SECONDS cmd...  : true as soon as cmd succeeds, false at the limit
    local limit=$1 i=0; shift
    while [ "$i" -lt "$((limit * 10))" ]; do
        "$@" && return 0
        i=$((i + 1)); sleep 0.1
    done
    return 1
}
window_id() {  # the app's window: owned by its pid, or titled for it
    wmctrl -lp 2>/dev/null | awk -v pid="$GUI_PID" '$3 == pid || tolower($0) ~ /tinyclient/ {print $1; exit}'
}
have_window() { [ -n "$(window_id)" ]; }
inbox_has_demo() {
    local r
    r=$(rows)
    [ "$(printf '%s\n' "$r" | grep -c .)" -ge 5 ] && printf '%s\n' "$r" | grep -q 'Yusuf Demirci'
}
frame_hash() { ffmpeg -nostdin -loglevel error -f x11grab -video_size 1280x800 -i "$DISPLAY" -frames:v 1 -f md5 - 2>/dev/null; }
hash_differs_from() { [ "$(frame_hash)" != "$1" ]; }
# A second launch carrying one quick action. Echoes its exit code, 125 if it never handed over
# and had to be killed (what a build without the actions does: it opens its own window).
action_launch() {
    local name=$1 pid i=0 rc
    "$APP/tinyclient" --action "$name" > "$SCRATCH/action-$name.log" 2>&1 &
    pid=$!
    while [ "$i" -lt 200 ] && alive "$pid"; do i=$((i + 1)); sleep 0.1; done
    if alive "$pid"; then
        kill -KILL "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
        echo 125
    else
        wait "$pid" 2>/dev/null
        echo $?
    fi
}

cleanup() {
    for pid in "$GUI_PID" "$CHATD_PID"; do
        if alive "$pid"; then kill -TERM "$pid" 2>/dev/null; fi
    done
    sleep 0.5
    for pid in "$GUI_PID" "$CHATD_PID"; do
        if alive "$pid"; then kill -KILL "$pid" 2>/dev/null; fi
    done
    # what the app logged, for the report; the scratch data goes with it
    [ -f "$SCRATCH/tinyclient.log" ] && cp "$SCRATCH/tinyclient.log" "$OUT/tinyclient.log"
    [ -f "$SCRATCH/chatd.log" ] && cp "$SCRATCH/chatd.log" "$OUT/chatd.log"
    case "$SCRATCH" in /tmp/tiny-e2e-*) rm -rf -- "$SCRATCH" ;; esac
}
trap cleanup EXIT

# 1. seeded demo data ready, so a signed-in window has somewhere to jump to.
"$APP/chatd" --seed-demo=live > "$SCRATCH/chatd.log" 2>&1 &
CHATD_PID=$!
if wait_for 60 inbox_has_demo; then
    step true "seeded demo data ready"
else
    step false "seeded demo data ready"
    exit 1
fi

# 2. the window is on screen and has settled, so a later picture change is the app's.
"$APP/tinyclient" > "$SCRATCH/tinyclient.log" 2>&1 &
GUI_PID=$!
if wait_for 45 have_window; then
    WIN=$(window_id)
    wmctrl -i -r "$WIN" -b add,maximized_vert,maximized_horz 2>/dev/null
    wmctrl -i -a "$WIN" 2>/dev/null
    sleep 2.5                                     # first frame, the inbox filling in
    step true "window is on screen" window
else
    step false "window is on screen" window
    exit 1
fi

# 3. New Message: the second launch hands the action over and exits; the switcher opens, so the
#    picture changes from the settled inbox.
BEFORE=$(frame_hash)
RC=$(action_launch new-message)
if [ "$RC" = 0 ]; then
    step true "New Message handed to the running window (second launch exits 0)"
else
    step false "New Message handed to the running window (second launch exit $RC)"
fi
if wait_for 8 hash_differs_from "$BEFORE"; then
    step true "the window shows the new-message switcher" switcher
else
    step false "the window shows the new-message switcher" switcher
fi

# 4. Inbox: handed over the same way.
RC=$(action_launch inbox)
step "$([ "$RC" = 0 ] && echo true || echo false)" "Inbox handed to the running window (second launch exit $RC)"

# 5. Quit: handed over, and the running app ends by itself with status 0 and no window.
RC=$(action_launch quit)
step "$([ "$RC" = 0 ] && echo true || echo false)" "Quit handed to the running window (second launch exit $RC)"
gone() { ! alive "$GUI_PID"; }
if wait_for 15 gone; then
    wait "$GUI_PID"; rc=$?
    GUI_PID=
    if [ "$rc" = 0 ] && ! have_window; then
        step true "the app quit (exit 0, window gone)"
    else
        step false "the app quit with exit $rc"
    fi
else
    step false "the app did not quit"
fi

[ "$FAILED" = 0 ]
