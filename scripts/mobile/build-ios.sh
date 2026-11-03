#!/usr/bin/env bash
# Builds tinyclient's Rust static library for iOS device + simulator, lipos the two simulator
# architectures into one slice, and writes mobile/ios/TinyCore.xcframework for XcodeGen.
#
#   scripts/mobile/build-ios.sh            # release
#   scripts/mobile/build-ios.sh --debug
#   scripts/mobile/build-ios.sh --debug --demo   # proof build: made-up chats from launch (`demo`)
#   scripts/mobile/build-ios.sh --debug --sim-only   # Apple-silicon simulator slice only (fast loop)
#   scripts/mobile/build-ios.sh --device-only        # device slice only, for a device archive (CI)
#   scripts/mobile/build-ios.sh --profile ci-mobile  # any Cargo profile; output is target/<triple>/<profile>
#
# Nothing here needs UniFFI: the shell calls one C symbol (`tinyclient_ios_main`). See
# docs/mobile-build.md. This script does NOT build the .app — that is xcodebuild's job, and the
# exact command is printed at the end.
#
# macOS only: lipo and xcodebuild ship with Xcode. On Windows there is no equivalent; use the
# `ios-app` job in .github/workflows/mobile.yml.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ios_dir="$root/mobile/ios"
xcframework="$ios_dir/TinyCore.xcframework"

profile="release"
profile_dir="release"
extra_args=(--release)
features="mobile"
sim_only=0
device_only=0

while [ $# -gt 0 ]; do
    case "$1" in
        --debug) profile="debug"; profile_dir="debug"; extra_args=() ;;
        --release) profile="release"; profile_dir="release"; extra_args=(--release) ;;
        --profile)
            [ $# -ge 2 ] || { echo "build-ios: --profile needs a name" >&2; exit 2; }
            profile="$2"; extra_args=(--profile "$2")
            # Cargo writes the dev profile to target/<triple>/debug and every other one to its own name.
            if [ "$2" = "dev" ]; then profile_dir="debug"; else profile_dir="$2"; fi
            shift ;;
        --demo) features="mobile demo" ;;
        --sim-only) sim_only=1 ;;
        --device-only) device_only=1 ;;
        -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build-ios: unknown argument: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

fail() {
    printf 'build-ios: %s\n' "$1" >&2
    exit 1
}

# The made-up chatd shows invented data and never reaches the real core: never in a release.
[ "$features" = "mobile" ] || [ "$profile" = "debug" ] || [ "$profile" = "dev" ] || fail "--demo is for debug proof builds only, never a release."
[ "$sim_only" = 0 ] || [ "$device_only" = 0 ] || fail "--sim-only and --device-only leave nothing to build together; pick one."

note() {
    printf 'build-ios: %s\n' "$1"
}

# --- host and tools --------------------------------------------------------

[ "$(uname -s)" = "Darwin" ] || fail "this script needs macOS: lipo and xcodebuild ship with Xcode, and the iOS SDK is macOS-only. On Windows use the ios-app job in .github/workflows/mobile.yml (a macos-26 runner)."

if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        fail "cargo is not on PATH. Install Rust from https://rustup.rs."
    fi
fi
command -v rustup >/dev/null 2>&1 || fail "rustup is not on PATH; cargo without rustup cannot add the iOS targets."
command -v xcodebuild >/dev/null 2>&1 || fail "xcodebuild is not on PATH. Install Xcode (26.6+; 27.2 is current) and run 'xcode-select --switch /Applications/Xcode.app'."
command -v lipo >/dev/null 2>&1 || fail "lipo is not on PATH. It ships with Xcode's command line tools: 'xcode-select --install'."

# --- Rust targets ----------------------------------------------------------

# x86_64-apple-ios is only for Intel Macs / Rosetta simulators, but the universal simulator
# slice is what `xcodebuild -create-xcframework` wants: it refuses two slices of the same
# platform+variant, so both simulator architectures must be lipo'd into one first.
targets=(aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios)
[ "$sim_only" = 1 ] && targets=(aarch64-apple-ios-sim)
# A device archive (`-destination 'generic/platform=iOS'`) links only the device slice.
[ "$device_only" = 1 ] && targets=(aarch64-apple-ios)
installed="$(rustup target list --installed)"
for triple in "${targets[@]}"; do
    if ! printf '%s\n' "$installed" | grep -qx "$triple"; then
        note "adding Rust target $triple"
        rustup target add "$triple"
    fi
done

# --- Build -----------------------------------------------------------------

# The app's floor (project.yml). Unset, the C dependencies and any dylib rustc links target
# iOS 10, where code built against the current SDK finds no ___chkstk_darwin.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

# `--lib` because the package also has the desktop binary; `mobile` is the entry point + gui.
# Xcode links only the static library, so `cargo rustc --crate-type staticlib` builds that one
# instead of every type in [lib] crate-type. Under release LTO each type is a separate link.
build() {
    local triple="$1"
    note "cargo rustc $profile --target $triple (staticlib)"
    (cd "$root" && cargo rustc "${extra_args[@]}" -p tinyclient --lib --features "$features" --target "$triple" --crate-type staticlib)
}

for triple in "${targets[@]}"; do
    build "$triple"
done

if [ "$sim_only" = 1 ]; then
    rm -rf "$xcframework"
    note "xcodebuild -create-xcframework (simulator slice only) -> $xcframework"
    xcodebuild -create-xcframework -library "$root/target/aarch64-apple-ios-sim/$profile_dir/libtinyclient.a" -output "$xcframework"
    exit 0
fi

device_lib="$root/target/aarch64-apple-ios/$profile_dir/libtinyclient.a"

if [ "$device_only" = 1 ]; then
    [ -f "$device_lib" ] || fail "expected $device_lib after the staticlib build."
    rm -rf "$xcframework"
    note "xcodebuild -create-xcframework (device slice only) -> $xcframework"
    xcodebuild -create-xcframework -library "$device_lib" -output "$xcframework"
    note "done: $(du -sh "$xcframework" | cut -f1) $xcframework"
    exit 0
fi

sim_arm_lib="$root/target/aarch64-apple-ios-sim/$profile_dir/libtinyclient.a"
sim_intel_lib="$root/target/x86_64-apple-ios/$profile_dir/libtinyclient.a"

for lib in "$device_lib" "$sim_arm_lib" "$sim_intel_lib"; do
    [ -f "$lib" ] || fail "expected $lib after the staticlib build."
done

mkdir -p "$root/target/universal-ios-sim"
universal_sim="$root/target/universal-ios-sim/libtinyclient.a"
note "lipo -> $universal_sim"
lipo -create "$sim_arm_lib" "$sim_intel_lib" -output "$universal_sim"

# --- XCFramework -----------------------------------------------------------

# xcodebuild refuses to overwrite an existing xcframework.
rm -rf "$xcframework"
note "xcodebuild -create-xcframework -> $xcframework"
xcodebuild -create-xcframework \
    -library "$device_lib" \
    -library "$universal_sim" \
    -output "$xcframework"

note "done: $(du -sh "$xcframework" | cut -f1) $xcframework"
if command -v xcodegen >/dev/null 2>&1; then
    note "next: (cd mobile/ios && xcodegen generate && xcodebuild -project Tiny.xcodeproj -scheme Tiny -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build)"
else
    note "next: install XcodeGen (brew install xcodegen), then run the command above."
fi
note "device archive for TestFlight needs a signing identity and team id; that is out of scope here."
