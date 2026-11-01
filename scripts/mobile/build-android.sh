#!/usr/bin/env bash
# Builds tinyclient's Rust library for Android and packages a debug APK.
#
#   scripts/mobile/build-android.sh                 # arm64-v8a + x86_64, debug APK
#   scripts/mobile/build-android.sh --release
#   scripts/mobile/build-android.sh --skip-gradle   # only the .so files
#
# Mirrors build-android.ps1. Every prerequisite is checked up front so a failure names the
# missing tool instead of surfacing as a linker error. See docs/mobile-build.md.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
android_dir="$root/mobile/android"
jni_libs="$android_dir/app/src/main/jniLibs"

profile="debug"
abis=("arm64-v8a" "x86_64")
skip_gradle=0

usage() {
    sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --release) profile="release" ;;
        --debug) profile="debug" ;;
        --skip-gradle) skip_gradle=1 ;;
        --abi) shift; abis=("$1") ;;
        -h|--help) usage; exit 0 ;;
        *) echo "build-android: unknown argument: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

fail() {
    printf 'build-android: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'build-android: %s\n' "$1"
}

# --- cargo, rustup ---------------------------------------------------------

if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        fail "cargo is not on PATH. Install Rust from https://rustup.rs, or add \$HOME/.cargo/bin to PATH."
    fi
fi
command -v rustup >/dev/null 2>&1 || fail "rustup is not on PATH; cargo without rustup cannot add the Android targets."

# bash 3.2 (macOS) has no associative arrays, so ABI -> triple is a case.
abi_triple() {
    case "$1" in
        arm64-v8a) printf 'aarch64-linux-android\n' ;;
        x86_64) printf 'x86_64-linux-android\n' ;;
        *) return 1 ;;
    esac
}

wanted=()
for abi in "${abis[@]}"; do
    triple="$(abi_triple "$abi")" || fail "unknown ABI '$abi' (use arm64-v8a or x86_64)"
    wanted+=("$triple")
done

installed="$(rustup target list --installed)"
missing=()
for triple in "${wanted[@]}"; do
    printf '%s\n' "$installed" | grep -qx "$triple" || missing+=("$triple")
done
if [ ${#missing[@]} -gt 0 ]; then
    fail "Rust targets not installed: ${missing[*]}. Run: rustup target add ${missing[*]}"
fi

# --- cargo-ndk -------------------------------------------------------------

cargo ndk --version >/dev/null 2>&1 || fail "cargo-ndk is not installed. Run: cargo install cargo-ndk --version 4.1.2 --locked"

# --- NDK -------------------------------------------------------------------

find_ndk() {
    for candidate in "${ANDROID_NDK_HOME:-}" "${ANDROID_NDK_ROOT:-}"; do
        if [ -n "$candidate" ] && [ -f "$candidate/source.properties" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    local sdk
    for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
        [ -n "$sdk" ] && [ -d "$sdk/ndk" ] || continue
        ls -1 "$sdk/ndk" | sort -V | tail -n 1 | sed "s|^|$sdk/ndk/|"
        return 0
    done
    return 1
}

ndk="$(find_ndk || true)"
[ -n "$ndk" ] || fail "No Android NDK found. Install NDK r30 (30.0.16248370) in Android Studio's SDK Manager, or set ANDROID_NDK_HOME."
export ANDROID_NDK_HOME="$ndk"
note "NDK $ndk"

# --- Rust library ----------------------------------------------------------

mkdir -p "$jni_libs"
cargo_args=(ndk)
for abi in "${abis[@]}"; do
    cargo_args+=(-t "$abi")
done
# cargo-ndk defaults to API 21, where libnativewindow is missing. Match minSdk in app/build.gradle.kts.
cargo_args+=(-o "$jni_libs" --platform 29 build)
[ "$profile" = "release" ] && cargo_args+=(--release)
# `--lib` because the package also has the desktop binary; `mobile` is the entry point + gui.
cargo_args+=(-p tinyclient --lib --features mobile)

note "cargo ${cargo_args[*]}"
(cd "$root" && cargo "${cargo_args[@]}")

for abi in "${abis[@]}"; do
    so="$jni_libs/$abi/libtinyclient.so"
    [ -f "$so" ] || fail "cargo ndk did not write $so (is [lib] crate-type missing cdylib?)"
    note "$so: $(du -h "$so" | cut -f1)"
done

if [ "$skip_gradle" -eq 1 ]; then
    note "skipping Gradle (--skip-gradle); jniLibs are in $jni_libs"
    exit 0
fi

# --- Gradle ----------------------------------------------------------------

command -v java >/dev/null 2>&1 || fail "java is not on PATH. AGP 9 needs JDK 17+ (Android Studio ships one)."
note "$(java -version 2>&1 | head -n 1)"

if [ -x "$android_dir/gradlew" ]; then
    gradle="$android_dir/gradlew"
elif command -v gradle >/dev/null 2>&1; then
    gradle="gradle"
else
    fail "No Gradle. mobile/android/gradlew is missing because the wrapper jar is not committed, and gradle is not on PATH. Run 'gradle wrapper --gradle-version 9.6.0' in mobile/android, or install Gradle 9.6+."
fi

(cd "$android_dir" && "$gradle" :app:assembleDebug --console=plain)

apk="$android_dir/app/build/outputs/apk/debug/app-debug.apk"
[ -f "$apk" ] || fail "Gradle reported success but $apk does not exist."
note "APK $apk"
note "install with: adb install -r \"$apk\" && adb shell am start -n dev.tinyclient.app/.TinyActivity"
note "16 KB check: llvm-readelf -lW <so> | grep LOAD  (every Align 0x4000), then zipalign -c -P 16 4 <apk>"
