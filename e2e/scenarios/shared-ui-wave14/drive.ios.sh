#!/bin/bash
# The lab's demo simulator build and XCTest runner are already installed.
set -eu
: "${E2E_OUT:?}" "${E2E_T0:?}" "${E2E_LAB:?}" "${E2E_OCR:?}" "${E2E_UDID:?}"
APP=dev.tinyclient.app
SHOT="${E2E_SHOTS:-$E2E_OUT}/wave14.png"
step() {
    local seconds
    seconds=$(perl -MTime::HiRes=time -e 'printf "%.2f", time - $ARGV[0]' "$E2E_T0")
    printf '{"t": %s, "step": "%s", "ok": %s}\n' "$seconds" "$1" "$2" >> "$E2E_OUT/steps.jsonl"
}
drive() {
    TEST_RUNNER_LAB_ACTIONS="$1" TEST_RUNNER_LAB_LOG="$E2E_OUT/actions.log" TEST_RUNNER_LAB_BUNDLE="$APP" \
        xcodebuild test-without-building -project "$E2E_LAB/ios-ui/TinyLabUI.xcodeproj" -scheme TinyLabUI \
        -destination "platform=iOS Simulator,id=$E2E_UDID" -derivedDataPath "$E2E_LAB/ios-ui-dd" \
        -only-testing:TinyLabUITests/LabActions/testActions >> "$E2E_OUT/runner.log" 2>&1
}
has_text() {
    xcrun simctl io "$E2E_UDID" screenshot --type=png "$SHOT" >/dev/null 2>&1
    "$E2E_OCR" "$SHOT" | grep -qiE "$1"
}
trap 'xcrun simctl terminate "$E2E_UDID" "$APP" >/dev/null 2>&1 || true' EXIT
xcrun simctl launch "$E2E_UDID" "$APP"
NAME="open a demo conversation and show the native attachment sources"
if drive "tap 0.6 0.33; wait 2; tap 0.55 0.50; wait 1; tap 0.09 0.94; wait 1" && has_text "Photos and videos" && has_text "Documents"; then
    step "$NAME" true
else
    step "$NAME" false
    exit 1
fi
