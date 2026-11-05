#!/bin/bash
# Host side of one Linux device (tiny-linux) in the device-link proof. Copied to ~/tiny-dl by
# `dl.py prepare-linux`; the orchestrator on the coordinator calls it over plain `ssh tiny-linux`.
#
#   linux-host.sh start <name> <session-b64> fresh|keep   chatd + window on :0, scratch dir /tmp/tiny-dl-<name>
#   linux-host.sh shot <name> <png>                        one still of :0
#   linux-host.sh rec-start <name> / rec-stop <name>       x11grab of :0 (the lab's recorder), into /tmp/tiny-dl-rec-<name>/screen.mp4
#   linux-host.sh stop <name>                              stops only what start started
#
# Invented people only: TINYCLIENT_DIR scratch mode never reads the real keyring or ~/.local/share.
set -u
WORK=$(cd "$(dirname "$0")" && pwd)
APP=$WORK/bin/app
export DISPLAY=:0 XAUTHORITY=/run/user/1000/gdm/Xauthority
cmd=$1 name=$2
DIR=/tmp/tiny-dl-$name          # short: chatd's socket path has a 108-byte limit
DATA=$DIR/data
REC=/tmp/tiny-dl-rec-$name   # apart from DIR: a fresh start wipes DIR

alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }

case "$cmd" in
start)
    sess=$(printf %s "$3" | base64 -d)
    if [ -f "$DIR/pids" ]; then while read -r p; do alive "$p" && kill -TERM "$p"; done < "$DIR/pids"; fi
    [ "$4" = fresh ] && rm -rf -- "$DIR"
    mkdir -p "$DATA/outbox" "$DIR/xdg-config" "$DIR/xdg-data" "$DIR/xdg-state"
    export TINYCLIENT_DIR=$DATA TINYCLIENT_LOG_LEVEL=debug RUST_LOG=info
    export XDG_CONFIG_HOME=$DIR/xdg-config XDG_DATA_HOME=$DIR/xdg-data XDG_STATE_HOME=$DIR/xdg-state
    # The variable's name has a space in it (the network label), which bash cannot export; env can.
    setsid env "TINYCLIENT_SESSION_TINY SERVER=$sess" "$APP/chatd" > "$DIR/chatd.log" 2>&1 < /dev/null &
    echo $! > "$DIR/pids"
    for _ in $(seq 1 100); do "$WORK/bin/dlctl" "$DATA" '{"op":"status"}' > /dev/null 2>&1 && break; sleep 0.2; done
    setsid "$APP/tinyclient" > "$DIR/tinyclient.log" 2>&1 < /dev/null &
    echo $! >> "$DIR/pids"
    gui=$!
    for _ in $(seq 1 150); do
        win=$(wmctrl -lp 2>/dev/null | awk -v pid="$gui" '$3 == pid {print $1; exit}')
        [ -n "$win" ] && break
        sleep 0.2
    done
    if [ -n "${win:-}" ]; then
        wmctrl -i -r "$win" -b add,maximized_vert,maximized_horz 2>/dev/null
        wmctrl -i -a "$win" 2>/dev/null
        echo "window $win"
    else
        echo "no window (chatd still answers)"
    fi
    "$WORK/bin/dlctl" "$DATA" '{"op":"status"}' > /dev/null 2>&1 || { echo "chatd not answering"; exit 1; }
    echo started
    ;;
shot)
    ffmpeg -nostdin -loglevel error -y -f x11grab -video_size 1280x800 -i "$DISPLAY" -frames:v 1 "$3"
    ;;
rec-start)
    mkdir -p "$REC"
    setsid ffmpeg -nostdin -loglevel info -y -f x11grab -framerate 10 -video_size 1280x800 -i "$DISPLAY" \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p "$REC/screen.mp4" > "$REC/ffmpeg.log" 2>&1 < /dev/null &
    echo $! > "$REC/rec.pid"
    for _ in $(seq 1 100); do grep -q 'frame=' "$REC/ffmpeg.log" 2>/dev/null && break; sleep 0.1; done
    date +%s.%N > "$REC/rec.t0"
    cat "$REC/rec.t0"
    ;;
rec-stop)
    if [ -f "$REC/rec.pid" ]; then
        p=$(cat "$REC/rec.pid")
        kill -INT "$p" 2>/dev/null
        for _ in $(seq 1 60); do alive "$p" || break; sleep 0.25; done
        alive "$p" && kill -KILL "$p"
        rm -f "$REC/rec.pid"
    fi
    ;;
stop)
    if [ -f "$DIR/pids" ]; then
        while read -r p; do alive "$p" && kill -TERM "$p"; done < "$DIR/pids"
        sleep 0.5
        while read -r p; do alive "$p" && kill -KILL "$p"; done < "$DIR/pids"
        rm -f "$DIR/pids"
    fi
    ;;
*) echo "unknown: $cmd" >&2; exit 2 ;;
esac
