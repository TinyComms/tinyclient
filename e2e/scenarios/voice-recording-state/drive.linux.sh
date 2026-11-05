#!/bin/bash
set -eu
: "${E2E_APP_DIR:?}" "${E2E_OUT:?}" "${E2E_T0:?}"
: "${E2E_MIC_X:?calibrate the composer microphone X coordinate}"
: "${E2E_MIC_Y:?calibrate the composer microphone Y coordinate}"
command -v tesseract >/dev/null
mkdir -p "$E2E_OUT/shots"
SCRATCH=$(mktemp -d /tmp/tiny-rec-e2e-XXXXXX)
export TINYCLIENT_DIR=$SCRATCH/data
export XDG_CONFIG_HOME=$SCRATCH/config XDG_DATA_HOME=$SCRATCH/share XDG_STATE_HOME=$SCRATCH/state
export TINYCLIENT_MEDIAD_AUDIO=tone
mkdir -p "$TINYCLIENT_DIR"
CHATD_PID= GUI_PID=
cleanup() {
    [ -z "$GUI_PID" ] || kill "$GUI_PID" 2>/dev/null || true
    [ -z "$CHATD_PID" ] || kill "$CHATD_PID" 2>/dev/null || true
}
trap cleanup EXIT
"$E2E_APP_DIR/chatd" --seed-demo=live >"$E2E_OUT/chatd.log" 2>&1 &
CHATD_PID=$!
for _ in $(seq 1 120); do
    if "$E2E_APP_DIR/tinyclient" --text 2>/dev/null | grep -q showcase; then break; fi
    sleep 0.5
done
"$E2E_APP_DIR/tinyclient" >"$E2E_OUT/tinyclient.log" 2>&1 &
GUI_PID=$!
for _ in $(seq 1 90); do
    WIN=$(wmctrl -lp | awk -v p="$GUI_PID" '$3 == p {print $1; exit}')
    [ -z "$WIN" ] || break
    sleep 0.5
done
[ -n "$WIN" ]
wmctrl -i -r "$WIN" -b add,maximized_vert,maximized_horz
wmctrl -i -a "$WIN"
sleep 2
python3 - <<'PY'
import ctypes as c
import json
import os
import subprocess as sp
import time
from pathlib import Path

x = c.CDLL('libX11.so.6')
t = c.CDLL('libXtst.so.6')
x.XOpenDisplay.restype = c.c_void_p
x.XOpenDisplay.argtypes = [c.c_char_p]
x.XStringToKeysym.restype = c.c_ulong
x.XStringToKeysym.argtypes = [c.c_char_p]
x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
x.XSync.argtypes = [c.c_void_p, c.c_int]
t.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
t.XTestFakeMotionEvent.argtypes = [c.c_void_p, c.c_int, c.c_int, c.c_int, c.c_ulong]
t.XTestFakeButtonEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
d = x.XOpenDisplay(None)
assert d, 'X11 display unavailable'
for name in [b'Down', b'Return']:
    key = x.XKeysymToKeycode(d, x.XStringToKeysym(name))
    for pressed in [1, 0]:
        t.XTestFakeKeyEvent(d, key, pressed, 0)
        x.XSync(d, 0)
    time.sleep(0.3)
time.sleep(2)

def click():
    t.XTestFakeMotionEvent(d, -1, int(os.environ['E2E_MIC_X']), int(os.environ['E2E_MIC_Y']), 0)
    for pressed in [1, 0]:
        t.XTestFakeButtonEvent(d, 1, pressed, 0)
        x.XSync(d, 0)
        time.sleep(0.1)

out = Path(os.environ['E2E_OUT'])
def expect(label, words):
    shot = out / 'shots' / (label + '.png')
    ok = False
    for _ in range(10):
        sp.run(['ffmpeg', '-nostdin', '-loglevel', 'error', '-y', '-f', 'x11grab',
                '-video_size', '1280x800', '-i', os.environ['DISPLAY'], '-frames:v', '1', str(shot)], check=True)
        text = sp.check_output(['tesseract', str(shot), 'stdout'], stderr=sp.DEVNULL).decode().lower()
        if all(word in text for word in words):
            ok = True
            break
        time.sleep(0.3)
    with (out / 'steps.jsonl').open('a') as f:
        f.write(json.dumps({'t': time.time() - float(os.environ['E2E_T0']),
                            'step': label, 'ok': ok, 'shot': 'shots/' + shot.name}) + '\n')
    assert ok, label + ': expected ' + repr(words) + ' in screen text: ' + text

click()
expect('recording-is-visible', ['recording', 'stop'])
time.sleep(2)
click()
expect('finished-note-is-staged', ['voice note', 'kept'])
PY
