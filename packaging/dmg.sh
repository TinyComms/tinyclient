#!/bin/sh
# Packs target/release/tinyclient.app (packaging/bundle.sh) into a disk image that opens on the
# app beside a link to Applications, for drag-to-install. The release workflow runs it on every
# v* tag (.github/workflows/release.yml). hdiutil only; no third-party tool.
#
#   packaging/dmg.sh dist/tinyclient-v0.2.1-macos.dmg
#   TARGET_DIR=/tmp/t packaging/dmg.sh OUT.dmg   the app under another target directory
#
# The app inside is signed ad hoc, as bundle.sh leaves it; the image itself is not signed.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
target=${TARGET_DIR:-$root/target}
app=$target/release/tinyclient.app
out=${1:?usage: packaging/dmg.sh OUT.dmg}

[ -d "$app" ] || { echo "dmg.sh: $app is not built (packaging/bundle.sh)" >&2; exit 1; }
codesign --verify --deep --strict "$app"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/tinyclient/tinyclient.app"
ln -s /Applications "$stage/tinyclient/Applications"

mkdir -p "$(dirname "$out")"
# hdiutil on a busy CI machine sometimes fails with "Resource busy"; a second try succeeds.
for attempt in 1 2 3; do
    if hdiutil create -volname tinyclient -srcfolder "$stage/tinyclient" -fs HFS+ -format ULMO -ov "$out"; then
        break
    fi
    [ $attempt = 3 ] && exit 1
    sleep 5
done
hdiutil verify "$out"
echo "$out"
