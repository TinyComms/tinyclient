#!/usr/bin/env bash
# The desktop half of the local Tiny call test, on a Linux VM with an X11 desktop (tiny-linux).
# Run by scripts/e2e/local-tiny-call.ps1 over ssh; nothing here touches the user's own desktop.
#
#   vm-desktop.sh build <src-dir>                 cargo build (stable) of tinyclient --features gui, chatd, mediad
#   vm-desktop.sh start <src-dir> <run-dir> <session-json> <join> [audio]
#                                                 tinyclient on DISPLAY=:0 as the session's person,
#                                                 TINYCLIENT_DEBUG_JOIN=<join>, test audio [tone:440],
#                                                 far-end audio to <run-dir>/played.pcm, ffmpeg
#                                                 x11grab of the screen to <run-dir>/screen.mp4
#   vm-desktop.sh stop <run-dir>                  stops what start started (pids in <run-dir>/pids)
set -eu
cmd=$1
case "$cmd" in
build)
    src=$2
    source "$HOME/.cargo/env"
    cd "$src"
    # Stable: nightly 2026-10-31 hits a query cycle in zvariant_utils (E0391).
    cargo +stable build -p tinyclient --features gui -p chatd -p mediad
    ;;
start)
    src=$2 run=$3 session=$4 join=$5 audio=${6:-tone:440}
    mkdir -p "$run/data"
    : > "$run/pids"
    export DISPLAY=:0 XAUTHORITY=/run/user/1000/gdm/Xauthority
    # mediad fetches OpenH264 from Cisco on first use, but a debug Linux build has no TLS backend
    # for that request ("Unknown Scheme"), so the screen share would never start. Fetch it here.
    h264="$HOME/.cache/tinyclient/libopenh264-2.6.0-linux64.8.so"
    if [ ! -s "$h264" ]; then
        mkdir -p "$(dirname "$h264")"
        curl -sfL https://ciscobinary.openh264.org/libopenh264-2.6.0-linux64.8.so.bz2 | bunzip2 > "$h264"
    fi
    # The screen, recorded for the proof, from before the client opens.
    ffmpeg -nostdin -loglevel error -y -f x11grab -framerate 10 -video_size "$(xdpyinfo | awk '/dimensions/{print $2}')" -i :0 \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p "$run/screen.mp4" > "$run/ffmpeg.log" 2>&1 &
    echo "ffmpeg $!" >> "$run/pids"
    # TINYCLIENT_DIR: no keyring, chatd takes the session from the variable below.
    env TINYCLIENT_DIR="$run/data" \
        "TINYCLIENT_SESSION_TINY SERVER=$session" \
        TINYCLIENT_MEDIAD_AUDIO="$audio" \
        TINYCLIENT_MEDIAD_SINK="$run/played.pcm" \
        TINYCLIENT_LOG_LEVEL=debug \
        RUST_LOG=debug \
        TINYCLIENT_DEBUG_JOIN="$join" \
        "$src/target/debug/tinyclient" > "$run/tinyclient.log" 2>&1 &
    echo "tinyclient $!" >> "$run/pids"
    echo "started: $(cat "$run/pids" | tr '\n' ' ')"
    ;;
stop)
    run=$2
    [ -f "$run/pids" ] || exit 0
    # ffmpeg first, with q, so the mp4 is finished; then the client and its children.
    while read -r name pid; do
        if [ "$name" = ffmpeg ]; then kill -INT "$pid" 2>/dev/null || true; fi
    done < "$run/pids"
    sleep 3
    while read -r name pid; do
        pkill -TERM -P "$pid" 2>/dev/null || true
        kill -TERM "$pid" 2>/dev/null || true
    done < "$run/pids"
    sleep 2
    pkill -KILL -f "$run/data" 2>/dev/null || true
    # chatd and mediad started by this client carry TINYCLIENT_DIR, not the path, in argv.
    for p in $(pgrep -x chatd; pgrep -x mediad); do
        if tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -qx "TINYCLIENT_DIR=$run/data"; then kill -KILL "$p"; fi
    done
    rm -f "$run/pids"
    echo stopped
    ;;
esac
