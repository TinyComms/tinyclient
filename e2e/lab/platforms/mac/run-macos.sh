#!/bin/sh
# On the Mac. Films the tinyclient window only while the scenario's driver runs, then stops the
# recorder. Never records a display: see helper/main.swift.
#   env: RUN (folder under /tmp/tiny-lab-runs holding tinyclient.app, scenario/ and out/)
set -u
. "$HOME/tiny-lab/lib.sh"
R=$RUNS/$RUN
OUT=$R/out
HELPER=$(helper_app)
rm -rf "$OUT"; mkdir -p "$OUT"
rm -f "$OUT/app.pid" "$OUT/t0" "$OUT/rec.done"

# No grant, no recording: say so before anything is started on the Mac.
open -W -n --stdout "$OUT/status.txt" --stderr "$OUT/status.txt" -a "$HELPER" --args status >/dev/null 2>&1
STATUS=$(cat "$OUT/status.txt" 2>/dev/null)
echo "HELPER_STATUS=$STATUS"
case "$STATUS" in
    *'"screen_recording": true'*) ;;
    *) echo "RESULT=norecorder"; exit 77 ;;
esac

open -n --stdout "$OUT/rec.log" --stderr "$OUT/rec.log" -a "$HELPER" --args record \
    --pid-file "$OUT/app.pid" --out "$OUT/recording.mp4" --ready "$OUT/t0" --done "$OUT/rec.done" --fps 10 --wait 150
i=0
while [ ! -s "$OUT/t0" ] && [ $i -lt 150 ]; do sleep 0.1; i=$((i + 1)); done
REC=$(pgrep -f "TinyLabHelper record .*$OUT/recording.mp4" | head -1)
if [ ! -s "$OUT/t0" ] || [ -z "$REC" ]; then
    echo "lab: the recorder did not start:"; cat "$OUT/rec.log" 2>/dev/null
    echo "RESULT=norecorder"; exit 77
fi

E2E_T0=$(cat "$OUT/t0")
export E2E_T0 E2E_OUT=$OUT E2E_APP_DIR=$R/tinyclient.app E2E_HELPER=$HELPER
bash "$R/scenario/drive.macos.sh" >"$OUT/driver.log" 2>&1
RC=$?

kill -INT "$REC" 2>/dev/null
i=0
while [ ! -f "$OUT/rec.done" ] && [ $i -lt 200 ]; do sleep 0.1; i=$((i + 1)); done
STOPPED=$(now)
pgrep -f "TinyLabHelper record .*$OUT/recording.mp4" >/dev/null && kill -KILL "$REC" 2>/dev/null
cat "$OUT/rec.log"
echo "DRIVER_RC=$RC"
echo "SECONDS=$(perl -e "printf '%.2f', $STOPPED - $E2E_T0")"
