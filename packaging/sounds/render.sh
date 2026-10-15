#!/bin/sh
# Renders the seven notification sounds from `mediad::cue` and encodes them as the committed CAF
# files (22.05 kHz mono IMA4), then writes the WAVs' hashes to SHA256SUMS. macOS only: afconvert.
#
#   packaging/sounds/render.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
wavs=$(mktemp -d)
cargo run -q -p mediad --example sounds --manifest-path "$root/Cargo.toml" -- "$wavs" >/dev/null
for wav in "$wavs"/*.wav; do
    name=$(basename "$wav" .wav)
    afconvert -f caff -d ima4 -c 1 "$wav" "$here/$name.caf"
done
# afconvert's padding and info strings play no part: about 4 KB a file.
cargo run -q -p mediad --example sounds --manifest-path "$root/Cargo.toml" -- strip "$here"/*.caf
(cd "$wavs" && shasum -a 256 ./*.wav | sed 's| \./| |') > "$here/SHA256SUMS"
rm -rf "$wavs"
du -ch "$here"/*.caf | tail -1
