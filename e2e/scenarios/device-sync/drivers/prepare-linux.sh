#!/bin/bash
# Builds the device-link proof's Linux device on tiny-linux, from a client checkout on the coordinator.
# Run from Git Bash on the coordinator:  bash drivers/prepare-linux.sh [client-checkout] [profile] [ref]
# (default: this client repository, release, HEAD). Only the committed `ref` is built (git archive). Same build as the lab's (e2e/lab/platforms/linux.py
# BUILD_SH: stable, -p tinyclient --features gui -p chatd -p mediad) into the lab's own warm target
# dir ~/tiny-lab/target; immutable results go to ~/tiny-dl/bin/apps/<full-sha>.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(dirname "$HERE")
CLIENT=${1:-$ROOT/../../..}
PROFILE=${2:-release}
REF=${3:-HEAD}
HOST=${DL_LINUX_HOST:-tiny-linux}
SHA=$(git -C "$CLIENT" rev-parse "$REF^{commit}")
SHORT=${SHA:0:12}
DIRTY=$(git -C "$CLIENT" status --porcelain --untracked-files=no | wc -l)
echo "client $CLIENT @ $SHORT ($REF; uncommitted changes in tracked files there: $DIRTY; only the commit is built)"
TAR=$(mktemp -u /tmp/dl-src-XXXX).tar
git -C "$CLIENT" -c core.autocrlf=false archive --format=tar -o "$TAR" "$SHA"
tar -rf "$TAR" -C "$ROOT" dlctl/Cargo.toml dlctl/src/main.rs drivers/linux-host.sh drivers/linux-processes.py drivers/x11input.py
# git archive stamps files with the commit's time; in the shared target dir an older commit's files
# would look already built next to a later one's artifacts. Stamp them "now" so cargo rebuilds them.
ssh "$HOST" "rm -rf ~/tiny-dl/src/$SHORT && mkdir -p ~/tiny-dl/src/$SHORT ~/tiny-dl/bin && tar -x -C ~/tiny-dl/src/$SHORT && find ~/tiny-dl/src/$SHORT -type f -exec touch {} +" < "$TAR"
rm -f "$TAR"
ssh "$HOST" bash -s -- "$SHORT" "$PROFILE" "$SHA" <<'REMOTE'
set -eu
short=$1 profile=$2 sha=$3
source "$HOME/.cargo/env"
src=$HOME/tiny-dl/src/$short
cd "$src"
[ -e crates/chatd/telegram.env ] || : > crates/chatd/telegram.env
export CARGO_TARGET_DIR=$HOME/tiny-lab/target
export CARGO_BUILD_JOBS=1
flag=--release; dir=release
[ "$profile" = dev ] && { flag=; dir=debug; }
cargo +stable build $flag -p tinyclient --features gui -p chatd -p mediad -p login
# dlctl against this checkout's own protocol crate
sed -i 's#path = "../../../../crates/protocol"#path = "../crates/protocol"#' dlctl/Cargo.toml
cp Cargo.lock dlctl/Cargo.lock
(cd dlctl && cargo +stable build --release)
app=$HOME/tiny-dl/bin/apps/$sha
if [ -e "$app" ]; then
    [ -f "$app/SOURCE_SHA" ] && [ "$(cat "$app/SOURCE_SHA")" = "$sha" ] || { echo "refusing to replace existing app directory: $app" >&2; exit 1; }
    for binary in tinyclient chatd mediad tinyclient-login; do
        [ -x "$app/$binary" ] || { echo "incomplete immutable app directory: $app" >&2; exit 1; }
    done
else
    mkdir -p "$app"
    cp "$CARGO_TARGET_DIR/$dir/tinyclient" "$CARGO_TARGET_DIR/$dir/chatd" "$CARGO_TARGET_DIR/$dir/mediad" "$CARGO_TARGET_DIR/$dir/tinyclient-login" "$app/"
    printf '%s\n' "$sha" > "$app/SOURCE_SHA"
fi
cp "$CARGO_TARGET_DIR/release/dlctl" ~/tiny-dl/bin/dlctl
cp drivers/linux-host.sh ~/tiny-dl/linux-host.sh
cp drivers/linux-processes.py ~/tiny-dl/linux-processes.py
cp drivers/x11input.py ~/tiny-dl/x11input.py
"$app/tinyclient" --version
echo "ready: $app ($sha, $profile); pass this path with --linux-app-dir"
REMOTE
