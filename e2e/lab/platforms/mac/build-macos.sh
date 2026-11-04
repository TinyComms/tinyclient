#!/bin/sh
# On the Mac. Builds the macOS app of one SHA the way a release does (release profile, gui +
# chatd + mediad + login) and assembles tinyclient.app with packaging/bundle.sh.
#   env: SRC_TAR (git archive of the SHA), RUN (name of this run's folder under /tmp/tiny-lab-runs)
set -eu
. "$HOME/tiny-lab/lib.sh"
lock
started=$(now)
sync_src "$SRC_TAR"
cd "$LAB/src"
mkdir -p "$RUNS"
if ! nice -n 10 cargo build --release -j "${JOBS:-6}" --manifest-path Cargo.toml --target-dir "$LAB/target"     -p tinyclient --features tinyclient/gui -p chatd -p mediad -p login >"$RUNS/$RUN.build.log" 2>&1; then
    tail -n 40 "$RUNS/$RUN.build.log"; echo "lab: cargo build failed" >&2; exit 1
fi
tail -n 3 "$RUNS/$RUN.build.log"
TARGET_DIR=$LAB/target sh packaging/bundle.sh --no-build >/dev/null
rm -rf "$RUNS/$RUN"; mkdir -p "$RUNS/$RUN"
ditto "$LAB/target/release/tinyclient.app" "$RUNS/$RUN/tinyclient.app"
echo "BINARY_SHA256=$(shasum -a 256 "$RUNS/$RUN/tinyclient.app/Contents/MacOS/tinyclient" | cut -d' ' -f1)"
echo "BUILD_SECONDS=$(perl -e "printf '%.1f', $(now) - $started")"
