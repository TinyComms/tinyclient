#!/bin/sh
# Rewrites cargoLock.outputHashes in packaging/nix/package.nix from Cargo.lock: one hash per git
# repository and rev, under the first crate Cargo.lock lists from it. Run it after a change moves a
# git dependency. Needs Nix and the network.
#
#   packaging/nix/update-hashes.sh
#
# The hashes come from nix-prefetch-git, which is what nixpkgs' fetchgit runs, with fetchgit's
# defaults (submodules, no history), from the nixpkgs flake.lock pins.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
package=$root/packaging/nix/package.nix
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

prefetch=$(nix build --inputs-from "$root" nixpkgs#nix-prefetch-git --no-link --print-out-paths)/bin/nix-prefetch-git

# name-version, URL and commit of the first crate from each git source, in Cargo.lock's order.
awk '
    /^\[\[package\]\]/ { name = ""; version = "" }
    /^name = /         { name = $3; gsub(/"/, "", name) }
    /^version = /      { version = $3; gsub(/"/, "", version) }
    /^source = "git\+/ {
        source = $3; gsub(/"/, "", source); sub(/^git\+/, "", source)
        rev = source; sub(/.*#/, "", rev)
        url = source; sub(/[?#].*/, "", url)
        if (!(rev in seen)) { seen[rev] = 1; print name "-" version, url, rev }
    }
' "$root/Cargo.lock" > "$work/sources"

while read -r crate url rev; do
    repo=$(echo "$url" | sed 's|^[a-z]*://[^/]*/||; s|\.git$||')
    echo "update-hashes.sh: $repo at $rev" >&2
    hash=$("$prefetch" --url "$url" --rev "$rev" --fetch-submodules --quiet | sed -n 's/^  "hash": "\(.*\)",$/\1/p')
    [ -n "$hash" ] || { echo "update-hashes.sh: no hash for $url at $rev" >&2; exit 1; }
    echo "        \"$crate\" = \"$hash\"; # $repo" >> "$work/entries"
done < "$work/sources"

awk -v entries="$work/entries" '
    !done && /outputHashes = \{/ { print; while ((getline line < entries) > 0) print line; skipping = 1; next }
    skipping && /^ *};/ { skipping = 0; done = 1 }
    !skipping { print }
' "$package" > "$work/package.nix"
cat "$work/package.nix" > "$package"
echo "update-hashes.sh: $(wc -l < "$work/entries") git sources in $package" >&2
