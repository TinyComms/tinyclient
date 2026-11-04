#!/bin/sh
# Builds ~/tiny-lab/bin/TinyLabHelper.app from main.swift, signed ad hoc. Rebuilds only when the
# source changed. Prints the app path. Run on the Mac.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
dest=${1:-$HOME/tiny-lab/bin}
app=$dest/TinyLabHelper.app
stamp=$app/Contents/Resources/source.sha
want=$(shasum -a 256 "$here/main.swift" "$here/Info.plist" | shasum -a 256 | cut -d' ' -f1)
if [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ]; then
    echo "$app"
    exit 0
fi
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$here/Info.plist" "$app/Contents/Info.plist"
# -swift-version 5: this file is a script with top-level state, not strict-concurrency code.
swiftc -O -swift-version 5 -parse-as-library -o "$app/Contents/MacOS/TinyLabHelper" "$here/main.swift" 2>/dev/null \
    || swiftc -O -swift-version 5 -o "$app/Contents/MacOS/TinyLabHelper" "$here/main.swift"
echo "$want" > "$stamp"
codesign --force --sign - --identifier dev.tinyclient.lab.helper "$app"
echo "$app"
