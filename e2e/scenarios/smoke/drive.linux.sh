#!/bin/bash
# smoke on Linux (X11), run ON the host by e2e/lab/platforms/linux.py while ffmpeg films :0.
#
# In:  E2E_OUT (steps.jsonl, shots and logs go here), E2E_T0 (epoch seconds when the recorder
#      started), E2E_APP_DIR (folder with tinyclient, chatd, mediad), E2E_SABOTAGE=1 (start the
#      app on an EMPTY data dir, so the inbox step fails), DISPLAY, XAUTHORITY.
# Out: exit 0 only if every step is ok. One JSON line per step in $E2E_OUT/steps.jsonl.
#
# All data is invented: chatd --seed-demo fills a scratch TINYCLIENT_DIR, and with TINYCLIENT_DIR
# set chatd never reads or writes the real store or credentials. XDG dirs point at scratch too.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
APP=${E2E_APP_DIR:?E2E_APP_DIR}
OUT=${E2E_OUT:?E2E_OUT}
T0=${E2E_T0:?E2E_T0}
SABOTAGE=${E2E_SABOTAGE:-}
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
    while [ "$i" -lt $((limit * 10)) ]; do
        "$@" && return 0
        i=$((i + 1)); sleep 0.1
    done
    return 1
}
window_id() {  # the app's window: owned by its pid, or titled for it
    wmctrl -lp 2>/dev/null | awk -v pid="$GUI_PID" '$3 == pid || tolower($0) ~ /tinyclient/ {print $1; exit}'
}
have_window() { [ -n "$(window_id)" ]; }
unread_cleared() { [ -z "$(unread_of showcase)" ]; }
inbox_has_demo() {
    local r
    r=$(rows)
    [ "$(printf '%s\n' "$r" | grep -c .)" -ge 5 ] && printf '%s\n' "$r" | grep -q 'Yusuf Demirci'
}
unread_of() {  # the unread count of the conversation titled $1, from `tinyclient --text` (title is columns 10-33, count 35-37)
    rows | grep -m1 "^[A-Za-z]* *$1 " | cut -c35-37 | tr -d ' '
}
frame_hash() { ffmpeg -nostdin -loglevel error -f x11grab -video_size 1280x800 -i "$DISPLAY" -frames:v 1 -f md5 - 2>/dev/null; }

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

# 1. seeded demo data ready: chatd serves invented data from the scratch dir; the window will
#    find it answering and use it, as it does for any chatd that is already up.
if [ -n "$SABOTAGE" ]; then
    "$APP/chatd" > "$SCRATCH/chatd.log" 2>&1 &
else
    "$APP/chatd" --seed-demo=live > "$SCRATCH/chatd.log" 2>&1 &
fi
CHATD_PID=$!
if [ -n "$SABOTAGE" ]; then
    if wait_for 30 text; then step true "scratch data dir ready and empty (sabotaged: not seeded)"; else step false "chatd did not start"; exit 1; fi
else
    if wait_for 60 inbox_has_demo; then step true "seeded demo data ready"; else step false "seeded demo data ready"; exit 1; fi
fi

# 2. the window is on screen
"$APP/tinyclient" > "$SCRATCH/tinyclient.log" 2>&1 &
GUI_PID=$!
if wait_for 45 have_window; then
    WIN=$(window_id)
    wmctrl -i -r "$WIN" -b add,maximized_vert,maximized_horz 2>/dev/null
    wmctrl -i -a "$WIN" 2>/dev/null
    sleep 1.5                                    # first frame, the inbox filling in
    step true "window is on screen" window
else
    step false "window is on screen" window
    exit 1
fi

# 3. the inbox lists the demo conversations: asserted from the app's own state (the chatd the
#    window is attached to, through `tinyclient --text`), then seen in the picture.
if wait_for 5 inbox_has_demo; then
    n=$(rows | grep -c .)
    step true "inbox lists $n demo conversations" inbox
else
    n=$(rows | grep -c .)
    step false "inbox is empty: $n conversations, no demo data" inbox
    INBOX_FAILED=1
fi

# 4. a conversation opens: the keyboard does what a person does (down to a row, Enter).
if [ -z "${INBOX_FAILED:-}" ]; then
    wmctrl -i -a "$WIN" 2>/dev/null
    # Pinned #showcase is the first row and arrives with one unread; opening it reads it, which
    # chatd reports in its inbox. The picture has to change too.
    unread_before=$(unread_of showcase)
    before=$(frame_hash)
    python3 "$HERE/x11input.py" key Down Return
    wait_for 5 unread_cleared
    sleep 1
    after=$(frame_hash)
    unread_after=$(unread_of showcase)
    if [ -n "$unread_before" ] && [ -z "$unread_after" ] && [ "$before" != "$after" ] && alive "$GUI_PID"; then
        step true "a conversation opens (showcase: $unread_before unread, then read)" conversation
    else
        step false "a conversation did not open (showcase unread: '$unread_before' then '$unread_after')" conversation
    fi
    # Then a person's walk down the list (Alt+Down opens the next row), so the camera sees the
    # app move; it asserts nothing the step above did not. Under a second apart on purpose: the
    # lab's recording check counts a picture that holds still for 1 s or more as frozen time.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        sleep 0.6
        python3 "$HERE/x11input.py" key alt+Down
    done
    sleep 0.6
fi

# 5. the app quits cleanly: Ctrl+Q, the process exits 0, the window is gone.
if alive "$GUI_PID"; then
    wmctrl -i -a "$WIN" 2>/dev/null
    python3 "$HERE/x11input.py" key ctrl+q
    gone() { ! alive "$GUI_PID"; }
    if wait_for 15 gone; then
        wait "$GUI_PID"; rc=$?
        GUI_PID=
        if [ "$rc" = 0 ] && ! have_window; then step true "app quits cleanly"; else step false "app quit with exit $rc"; fi
    else
        step false "app did not quit"
    fi
else
    step false "app quits cleanly (it had already exited)"
fi

[ "$FAILED" = 0 ]
