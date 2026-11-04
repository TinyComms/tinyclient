#!/bin/bash
# smoke on iOS. Runs ON the Mac (the lab ships it with the scenario), under e2e/lab/platforms/ios.py,
# against the iPhone simulator that run-ios.sh booted and installed the app on.
#
# Invented data: the app is a `demo` build, whose made-up chatd (shot_demo.rs) is inside the app and
# on from launch, so no account and no data folder is needed. E2E_SABOTAGE=1 launches it with
# `--demo-empty` (the same fake core with nothing signed in), so the inbox step must fail.
#
# What is asserted: the process is alive, and the text on the simulator's screen (Vision OCR of
# `simctl io screenshot`) is what the app should show: the demo's conversation titles in the inbox,
# then the open conversation's composer. GPUI draws its own pixels, so there is no accessibility
# tree to read; the app has no log on iOS either. Taps come from the lab's UI-test runner
# (platforms/mac/ios-ui), which taps at fractions of the screen and needs no macOS permission.
#
# env from the lab: E2E_T0, E2E_OUT, E2E_APP_DIR, E2E_UDID, E2E_LAB (~/tiny-lab), E2E_OCR (the reader).
set -u
: "${E2E_T0:?}" "${E2E_OUT:?}" "${E2E_UDID:?}" "${E2E_LAB:?}" "${E2E_OCR:?}"
export DEVELOPER_DIR=${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}
APPID=dev.tinyclient.app
UDID=$E2E_UDID
SHOTS=${E2E_SHOTS:-$E2E_OUT}   # simctl writes only to the internal disk (see run-ios.sh)
STEPS=$E2E_OUT/steps.jsonl
: > "$STEPS"
touch "$E2E_OUT/start.ref"
FAILED=0

step() {  # step "words" true|false [epoch the thing happened]
    local at=${3:-}
    [ -n "$at" ] || at=$(perl -MTime::HiRes=time -e 'print time')
    local t
    t=$(perl -e 'printf "%.2f", $ARGV[0] - $ARGV[1]' "$at" "$E2E_T0")
    printf '{"t": %s, "step": "%s", "ok": %s}\n' "$t" "$1" "$2" >> "$STEPS"
    [ "$2" = true ] || FAILED=1
}
screen_text() {  # screen_text name: screenshot + OCR into $E2E_OUT/name.{png,txt}
    xcrun simctl io "$UDID" screenshot --type=png "$SHOTS/$1.png" >/dev/null 2>&1
    "$E2E_OCR" "$SHOTS/$1.png" > "$E2E_OUT/$1.txt" 2>/dev/null
}
# wait_for name "pattern" seconds: poll the screen until the OCR text matches
wait_for() {
    local end=$(( $(date +%s) + $3 ))
    while [ "$(date +%s)" -lt "$end" ]; do
        screen_text "$1"
        grep -qiE "$2" "$E2E_OUT/$1.txt" && return 0
        sleep 1
    done
    return 1
}

# 1. Invented data: the demo core is compiled into the installed app.
if xcrun simctl get_app_container "$UDID" "$APPID" >/dev/null 2>&1 && grep -rqa "Foray carpool" "$E2E_APP_DIR"; then
    step "demo data is built into the installed app" true
else
    step "demo data is built into the installed app" false
fi

# 2. Launch.
ARGS=""; [ "${E2E_SABOTAGE:-0}" = 1 ] && ARGS="--demo-empty"
LAUNCH=$(xcrun simctl launch "$UDID" "$APPID" $ARGS 2>&1)
PID=$(echo "$LAUNCH" | sed -n 's/^[^:]*: *\([0-9][0-9]*\)$/\1/p' | tail -1)
echo "launch: $LAUNCH" >> "$E2E_OUT/driver-notes.txt"
if [ -n "$PID" ] && xcrun simctl spawn "$UDID" launchctl list 2>/dev/null | grep -q "$APPID"; then
    step "app launched on the simulator" true
else
    step "app launched on the simulator" false
fi

# 3. The inbox: the demo's own conversation titles are on screen.
if wait_for inbox "Foray carpool" 30 && grep -qi "Jump to" "$E2E_OUT/inbox.txt" && grep -qiE "Yusuf Demirci|Priya Raman" "$E2E_OUT/inbox.txt"; then
    step "inbox lists the demo conversations" true
else
    step "inbox lists the demo conversations" false
fi
sleep 1.5

# 4. Open the first conversation (Foray carpool, the top row under "Today").
rm -f "$E2E_OUT/actions.log"
OPENED=false
if [ "${E2E_SABOTAGE:-0}" != 1 ]; then
    TEST_RUNNER_LAB_ACTIONS="tap 0.6 0.33; wait 2" TEST_RUNNER_LAB_LOG="$E2E_OUT/actions.log" TEST_RUNNER_LAB_BUNDLE=$APPID \
        xcodebuild test-without-building -project "$E2E_LAB/ios-ui/TinyLabUI.xcodeproj" -scheme TinyLabUI \
        -destination "platform=iOS Simulator,id=$UDID" -derivedDataPath "$E2E_LAB/ios-ui-dd" \
        -only-testing:TinyLabUITests/LabActions/testActions >"$E2E_OUT/runner.log" 2>&1
    TAP=$(awk -F'\t' '/\ttap /{print $1; exit}' "$E2E_OUT/actions.log" 2>/dev/null)
    if [ -n "$TAP" ] && wait_for conversation "Message Foray|Foray carpool.*|Tomasz Brandt|Ines Marchetti" 10 \
        && grep -qi "Message" "$E2E_OUT/conversation.txt" && ! grep -qi "Jump to" "$E2E_OUT/conversation.txt"; then
        OPENED=true
    fi
fi
step "first conversation opens" $OPENED "${TAP:-}"

# 5. Quit: stop the app, and it must not have crashed on the way (no new crash report for it).
sleep 1
CRASHED=$(find "$HOME/Library/Logs/DiagnosticReports" -maxdepth 1 -name 'Tiny-*.ips' -newer "$E2E_OUT/start.ref" 2>/dev/null | head -1)
ALIVE=false; xcrun simctl spawn "$UDID" launchctl list 2>/dev/null | grep -q "$APPID" && ALIVE=true
if $ALIVE && [ -z "$CRASHED" ] && xcrun simctl terminate "$UDID" "$APPID" >/dev/null 2>&1; then
    step "app quits cleanly" true
else
    echo "alive=$ALIVE crash=$CRASHED" >> "$E2E_OUT/driver-notes.txt"
    step "app quits cleanly" false
fi
sleep 1

[ "$FAILED" = 0 ]
