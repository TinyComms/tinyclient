#!/bin/sh
# On the Mac. Builds the iOS simulator app of one SHA with the made-up chatd inside it (`demo`:
# invented people from launch, no accounts, nothing stored), then the lab's input runner.
#   env: SRC_TAR (git archive of the SHA), RUN (this run's folder under /tmp/tiny-lab-runs)
set -eu
. "$HOME/tiny-lab/lib.sh"
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
lock
started=$(now)
sync_src "$SRC_TAR"
mkdir -p "$RUNS"
LOGF=$RUNS/$RUN.build.log
cd "$LAB/src"
# Rust: the simulator slice only (debug: `--demo` refuses a release). Incremental in the shared target.
if ! nice -n 10 scripts/mobile/build-ios.sh --debug --demo --sim-only >"$LOGF" 2>&1; then
    tail -n 40 "$LOGF"; echo "lab: the Rust build for the iOS simulator failed" >&2; exit 1
fi
cd mobile/ios
xcodegen generate >>"$LOGF" 2>&1
if ! xcodebuild -project Tiny.xcodeproj -scheme Tiny -destination 'generic/platform=iOS Simulator' -configuration Debug \
    -derivedDataPath "$LAB/ios-dd" ARCHS=arm64 ONLY_ACTIVE_ARCH=NO CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=NO build >>"$LOGF" 2>&1; then
    grep -E "error:|warning: .*(ld|link)" "$LOGF" | tail -n 20; tail -n 15 "$LOGF"; echo "lab: xcodebuild failed" >&2; exit 1
fi
rm -rf "$RUNS/$RUN"; mkdir -p "$RUNS/$RUN"
ditto "$LAB/ios-dd/Build/Products/Debug-iphonesimulator/Tiny.app" "$RUNS/$RUN/Tiny.app"

# The input runner (a UI test that taps the app) and the screenshot reader; both change rarely.
cd "$LAB/ios-ui"
xcodegen generate >>"$LOGF" 2>&1
xcodebuild build-for-testing -project TinyLabUI.xcodeproj -scheme TinyLabUI -destination 'generic/platform=iOS Simulator' \
    -derivedDataPath "$LAB/ios-ui-dd" >>"$LOGF" 2>&1 || { tail -n 30 "$LOGF"; echo "lab: the input runner did not build" >&2; exit 1; }
ocr_tool >/dev/null
echo "BINARY_SHA256=$(shasum -a 256 "$RUNS/$RUN/Tiny.app/Tiny" | cut -d' ' -f1)"
echo "BUILD_SECONDS=$(perl -e "printf '%.1f', $(now) - $started")"
