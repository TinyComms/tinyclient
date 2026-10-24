#!/bin/sh
# Assembles tinyclient.app: the window, chatd, mediad and the sign-in helper in one bundle, signed
# ad hoc. A bundle is what gives tinyclient its own line in System Settings > Privacy & Security >
# Microphone: macOS names, and asks on behalf of, the app a process belongs to, and reads the
# reason from that app's Info.plist. Run from a terminal, the unbundled binaries borrow the
# terminal's permission instead.
#
#   packaging/bundle.sh                 build --release into target/, assemble target/release/tinyclient.app
#   TARGET_DIR=/tmp/t packaging/bundle.sh   the same under another target directory
#   packaging/bundle.sh --no-build      assemble from binaries that are already built
#
# Every executable goes in Contents/MacOS, side by side: each finds the others next to itself
# (`current_exe().with_file_name(..)`), exactly as in target/.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
target=${TARGET_DIR:-$root/target}
profile=release
app=$target/$profile/tinyclient.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

if [ "${1:-}" != "--no-build" ]; then
    # chatd embeds the Telegram app id at compile time. A release without it cannot sign in.
    # --no-build only packs binaries that were already compiled, which is what CI does after
    # the release build step has already refused a chatd with no id.
    if [ -z "${TINYCLIENT_TELEGRAM_API_ID:-}" ] || [ -z "${TINYCLIENT_TELEGRAM_API_HASH:-}" ]; then
        env_file=${TINYCLIENT_TELEGRAM_ENV:-"$HOME/.config/tinyclient/telegram.env"}
        if [ -f "$env_file" ]; then
            set -a
            # shellcheck disable=SC1090
            . "$env_file"
            set +a
        fi
    fi
    if [ -z "${TINYCLIENT_TELEGRAM_API_ID:-}" ] || [ -z "${TINYCLIENT_TELEGRAM_API_HASH:-}" ]; then
        echo "bundle.sh: set TINYCLIENT_TELEGRAM_API_ID and TINYCLIENT_TELEGRAM_API_HASH before building" >&2
        exit 1
    fi
    cargo build --release -j "${JOBS:-6}" --manifest-path "$root/Cargo.toml" --target-dir "$target" \
        -p tinyclient --features tinyclient/gui -p chatd -p mediad -p login
fi

for binary in tinyclient chatd mediad tinyclient-login; do
    [ -x "$target/$profile/$binary" ] || { echo "bundle.sh: $target/$profile/$binary is not built" >&2; exit 1; }
done

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
for binary in tinyclient chatd mediad tinyclient-login; do
    cp "$target/$profile/$binary" "$app/Contents/MacOS/$binary"
done
sed "s/@VERSION@/$version/g" "$root/packaging/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# The icon: packaging/icon.png is the 1024 px master, drawn from icon.svg. Spotlight and the Dock
# show 16 and 32, where that drawing blurs, so those (and their @2x) come from icon-small.png,
# drawn from icon-small.svg with a larger bubble and heavier letters. Re-render the PNGs when an SVG changes.
iconset=$(mktemp -d)/tinyclient.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    master=$root/packaging/icon.png
    [ $size -le 32 ] && master=$root/packaging/icon-small.png
    sips -z $size $size "$master" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$master" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/tinyclient.icns"
rm -rf "$(dirname "$iconset")"

# Ad hoc: no certificate, so it runs on this Mac only and macOS re-asks for the microphone when
# the binary changes. The helpers are signed first; the bundle's signature seals them. mediad is
# the process that opens the microphone and the app is the one macOS asks about, so those two
# carry the audio-input entitlement (it matters once the hardened runtime is turned on for
# distribution; without it the entitlement is inert).
entitlements=$root/packaging/entitlements.plist
codesign --force --sign - --identifier dev.tinyclient.chatd "$app/Contents/MacOS/chatd"
codesign --force --sign - --identifier dev.tinyclient.login "$app/Contents/MacOS/tinyclient-login"
codesign --force --sign - --identifier dev.tinyclient.mediad --entitlements "$entitlements" "$app/Contents/MacOS/mediad"
codesign --force --sign - --entitlements "$entitlements" "$app"
codesign --verify --deep --strict "$app"

echo "$app"
