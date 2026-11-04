# Sourced by the Mac-side lab scripts. Everything here runs on the Mac (mbp), as itz4blitz.
# PATH: cargo, Homebrew; the full Xcode for the iOS simulator tools.
export PATH="$PATH:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
LAB=$HOME/tiny-lab
# One folder per run (a copy of the app, the scenario, what it wrote). On the internal disk, not under
# $LAB: that is on the external drive, where the Mac's security agent hashes every new executable and
# can stall a big copy for minutes. The Rust target and the source stay on the external one.
RUNS=/tmp/tiny-lab-runs
export CARGO_TARGET_DIR=$LAB/target

now() { perl -MTime::HiRes=time -e 'printf "%.3f", time'; }

# One build at a time on this Mac: the source tree and the cargo target are shared. Waits up to
# 30 minutes for another lab run, and takes the lock over from a dead one.
lock() {
    mkdir -p "$LAB"
    i=0
    while ! mkdir "$LAB/.lock" 2>/dev/null; do
        holder=$(cat "$LAB/.lock/pid" 2>/dev/null || echo 0)
        if [ "$holder" != 0 ] && ! kill -0 "$holder" 2>/dev/null; then rm -rf "$LAB/.lock"; continue; fi
        i=$((i + 1)); [ $i -gt 1800 ] && { echo "lab: the Mac's build lock is held by pid $holder" >&2; exit 75; }
        sleep 1
    done
    echo $$ > "$LAB/.lock/pid"
    trap 'rm -rf "$LAB/.lock"' EXIT
}

# Source of one SHA into $LAB/src, touching only files that changed, so cargo keeps its caches.
# $1 = tar made by `git archive`.
sync_src() {
    rm -rf "$LAB/incoming"; mkdir -p "$LAB/incoming" "$LAB/src" "$LAB/target"
    tar -xf "$1" -C "$LAB/incoming"
    rsync -rlc --delete --exclude /target --exclude /mobile/ios/TinyCore.xcframework \
        --exclude /mobile/ios/Tiny.xcodeproj --exclude /mobile/ios/build "$LAB/incoming/" "$LAB/src/"
    [ -e "$LAB/src/target" ] || ln -s ../target "$LAB/src/target"
    rm -rf "$LAB/incoming"
}

# The helper app (window-only recorder and input), built once from the lab's own source.
helper_app() {
    sh "$LAB/helper-src/build.sh" "$LAB/bin" | tail -1
}

# The screenshot reader (Vision), built once.
ocr_tool() {
    mkdir -p "$LAB/bin"
    if [ ! -x "$LAB/bin/ocr" ] || [ "$LAB/ocr.swift" -nt "$LAB/bin/ocr" ]; then
        swiftc -O -o "$LAB/bin/ocr" "$LAB/ocr.swift" 2>/dev/null
    fi
    echo "$LAB/bin/ocr"
}
