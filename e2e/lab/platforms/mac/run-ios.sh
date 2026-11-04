#!/bin/sh
# On the Mac. Films the iPhone simulator's own screen (simctl io recordVideo: the simulator only,
# never the Mac's display) while the scenario's drive.ios.sh runs.
#   env: RUN (folder under /tmp/tiny-lab-runs with Tiny.app and scenario/), DEVICE (simulator name)
set -u
. "$HOME/tiny-lab/lib.sh"
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
R=$RUNS/$RUN
OUT=$R/out
rm -rf "$OUT"; mkdir -p "$OUT"
# simctl's recorder and screenshots are written by CoreSimulator, which hangs (and wedges the
# simulator's display until it is rebooted) on a path on the external drive: ~/tiny-lab is a symlink
# to it. Everything simctl writes goes to the internal /tmp and is copied into out/ afterwards.
SCR=$(mktemp -d /tmp/lab-ios.XXXXXX)
BOOTED_BY_US=0 REC=""

UDID=$(xcrun simctl list devices available | grep -F "    $DEVICE (" | grep -oE '[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}' | head -1)
if [ -z "$UDID" ]; then echo "lab: no simulator named $DEVICE"; echo "RESULT=nodevice"; exit 78; fi
if ! xcrun simctl list devices | grep "$UDID" | grep -q Booted; then
    xcrun simctl boot "$UDID" && BOOTED_BY_US=1
    xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1
fi
echo "UDID=$UDID BOOTED_BY_US=$BOOTED_BY_US"

cleanup() {
    [ -n "$REC" ] && kill -0 "$REC" 2>/dev/null && kill -KILL "$REC" 2>/dev/null
    xcrun simctl terminate "$UDID" dev.tinyclient.app >/dev/null 2>&1
    xcrun simctl uninstall "$UDID" dev.tinyclient.app >/dev/null 2>&1
    [ "$BOOTED_BY_US" = 1 ] && xcrun simctl shutdown "$UDID" >/dev/null 2>&1
    rm -rf "$SCR"
    true
}
trap cleanup EXIT

# A clean slate: no earlier copy of the app (and its state) on this simulator.
xcrun simctl terminate "$UDID" dev.tinyclient.app >/dev/null 2>&1
xcrun simctl uninstall "$UDID" dev.tinyclient.app >/dev/null 2>&1
xcrun simctl install "$UDID" "$R/Tiny.app" || { echo "lab: install failed"; echo "RESULT=noinstall"; exit 79; }

# Recorder first, then the clock. simctl writes nothing until the first frame, and frames come only
# when the screen changes, so the recorder is started, then the screen is changed (Settings opens,
# or closes if it was already open), and E2E_T0 is the instant simctl says "Recording started".
# A screenshot first: the display's capture path is only live after something has asked for a frame;
# a recorder started on a display nobody has asked for waits for ever and then wedges the simulator.
perl -e 'alarm 30; exec @ARGV' xcrun simctl io "$UDID" screenshot --type=png "$SCR/start.png" >/dev/null 2>&1     || { echo "lab: the simulator's display does not answer (wedged: shut it down and boot it again)"; echo "RESULT=nodisplay"; exit 80; }
# rec.py runs simctl on a pseudo-terminal (so "Recording started" is not held back in a buffer) and
# passes the INT that ends the recording on to it. `--codec=` needs its equals sign.
python3 "$LAB/rec.py" "$UDID" "$SCR/recording.mp4" "$OUT/rec.log" </dev/null >/dev/null 2>&1 &
REC=$!
# The recorder takes a moment to attach, and a change before that is missed: keep changing the
# screen (Settings opens, closes, opens...) every 1.5 s until it reports the first frame.
i=0
while ! grep -q "Recording started" "$OUT/rec.log" 2>/dev/null && [ $i -lt 12 ]; do
    sleep 1.5
    if [ $((i % 2)) = 0 ]; then xcrun simctl launch "$UDID" com.apple.Preferences >/dev/null 2>&1; else xcrun simctl terminate "$UDID" com.apple.Preferences >/dev/null 2>&1; fi
    i=$((i + 1))
done
sleep 0.3
E2E_T0=$(now)
if ! grep -q "Recording started" "$OUT/rec.log"; then
    echo "lab: the recorder did not start"; cat "$OUT/rec.log"
    kill -INT "$REC" 2>/dev/null; sleep 2; kill -0 "$REC" 2>/dev/null && kill -KILL "$REC" 2>/dev/null
    REC=""; echo "RESULT=norecorder"; exit 77
fi
xcrun simctl terminate "$UDID" com.apple.Preferences >/dev/null 2>&1

export E2E_T0 E2E_SHOTS=$SCR E2E_OUT=$OUT E2E_APP_DIR=$R/Tiny.app E2E_UDID=$UDID E2E_LAB=$LAB E2E_OCR=$(ocr_tool)
bash "$R/scenario/drive.ios.sh" >"$OUT/driver.log" 2>&1
RC=$?

kill -INT "$REC" 2>/dev/null
i=0
while kill -0 "$REC" 2>/dev/null && [ $i -lt 300 ]; do sleep 0.1; i=$((i + 1)); done
cat "$OUT/rec.log"
STOPPED=$(now)
kill -0 "$REC" 2>/dev/null && kill -KILL "$REC" 2>/dev/null
REC=""
mv "$SCR/recording.mp4" "$OUT/recording.mp4" 2>/dev/null
cp "$SCR"/*.png "$OUT/" 2>/dev/null
echo "DRIVER_RC=$RC"
echo "SECONDS=$(perl -e "printf '%.2f', $STOPPED - $E2E_T0")"
