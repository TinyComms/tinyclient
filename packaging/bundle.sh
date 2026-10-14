#!/bin/sh
# Assembles tinychat.app: the window, chatd, mediad and the sign-in helper in one bundle, signed
# ad hoc. A bundle is what gives tinychat its own line in System Settings > Privacy & Security >
# Microphone: macOS names, and asks on behalf of, the app a process belongs to, and reads the
# reason from that app's Info.plist. Run from a terminal, the unbundled binaries borrow the
# terminal's permission instead.
#
#   packaging/bundle.sh                 build --release into target/, assemble target/release/tinychat.app
#   TARGET_DIR=/tmp/t packaging/bundle.sh   the same under another target directory
#   packaging/bundle.sh --no-build      assemble from binaries that are already built
#
# Every executable goes in Contents/MacOS, side by side: each finds the others next to itself
# (`current_exe().with_file_name(..)`), exactly as in target/.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
target=${TARGET_DIR:-$root/target}
profile=release
app=$target/$profile/tinychat.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

if [ "${1:-}" != "--no-build" ]; then
    cargo build --release -j "${JOBS:-6}" --manifest-path "$root/Cargo.toml" --target-dir "$target" \
        -p tinychat --features tinychat/gui -p chatd -p mediad -p login
fi

for binary in tinychat chatd mediad tinychat-login; do
    [ -x "$target/$profile/$binary" ] || { echo "bundle.sh: $target/$profile/$binary is not built" >&2; exit 1; }
done

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
for binary in tinychat chatd mediad tinychat-login; do
    cp "$target/$profile/$binary" "$app/Contents/MacOS/$binary"
done
sed "s/@VERSION@/$version/g" "$root/packaging/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# The icon is a placeholder (packaging/icon.png, 1024 px) until the real one is drawn.
iconset=$(mktemp -d)/tinychat.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z $size $size "$root/packaging/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$root/packaging/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/tinychat.icns"
rm -rf "$(dirname "$iconset")"

# Ad hoc: no certificate, so it runs on this Mac only and macOS re-asks for the microphone when
# the binary changes. The helpers are signed first; the bundle's signature seals them. mediad is
# the process that opens the microphone and the app is the one macOS asks about, so those two
# carry the audio-input entitlement (it matters once the hardened runtime is turned on for
# distribution; without it the entitlement is inert).
entitlements=$root/packaging/entitlements.plist
codesign --force --sign - --identifier dev.tinychat.chatd "$app/Contents/MacOS/chatd"
codesign --force --sign - --identifier dev.tinychat.login "$app/Contents/MacOS/tinychat-login"
codesign --force --sign - --identifier dev.tinychat.mediad --entitlements "$entitlements" "$app/Contents/MacOS/mediad"
codesign --force --sign - --entitlements "$entitlements" "$app"
codesign --verify --deep --strict "$app"

echo "$app"
