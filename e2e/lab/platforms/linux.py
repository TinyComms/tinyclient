"""Linux adapter: builds the app on tiny-linux (an X11 desktop VM) and drives it there while ffmpeg
x11grab films the VM's own screen. e2e/README.md is the contract; this file is the Linux half.

  1. the source tar goes to <work>/src/<sha12> (once per SHA);
  2. one cargo build into <work>/target, shared by every SHA, so a rebuild is incremental;
  3. the scenario's folder and the session script go to <work>/runs/<id>;
  4. the session script starts the recorder, takes E2E_T0 when the first frame is grabbed, runs
     drive.linux.sh, stops the recorder with SIGINT, and kills anything carrying its E2E_RUN_ID;
  5. out/ comes back into ctx.out_dir.

Optional keys under "linux" in hosts.json: "profile" ("release" default; "dev" for a debug build),
"driver_timeout" (seconds, default 300), "build_timeout" (seconds, default 3600).
Standard library only.
"""
import io
import os
import shlex
import subprocess
import sys
import tarfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import common  # noqa: E402

SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30"]
KEEP_SOURCES = 4

# Runs on the VM. `session` is the whole recorded run; `cleanup` kills by marker only.
SESSION_SH = r'''#!/bin/bash
# lab-run.sh session <run-dir> <app-dir> <driver-timeout>   |   lab-run.sh cleanup <run-id>
# Nothing here is killed by name: only processes whose environment carries this run's E2E_RUN_ID.
marked() {  # pids whose environment holds E2E_RUN_ID=$1, never this shell
    local d p
    for d in /proc/[0-9]*; do
        p=${d#/proc/}
        [ "$p" = "$$" ] && continue
        if { tr '\0' '\n' < "$d/environ"; } 2>/dev/null | grep -qx "E2E_RUN_ID=$1"; then echo "$p"; fi
    done
}
sweep() {
    local pids
    pids=$(marked "$1")
    [ -n "$pids" ] && kill -TERM $pids 2>/dev/null
    for _ in 1 2 3 4 5 6 7 8; do
        pids=$(marked "$1"); [ -z "$pids" ] && return 0; sleep 0.25
    done
    kill -KILL $pids 2>/dev/null
    return 0
}
if [ "$1" = cleanup ]; then sweep "$2"; exit 0; fi

RUN=$2; APP=$3; LIMIT=${4:-300}
ID=$(basename "$RUN")
OUT=$RUN/out
mkdir -p "$OUT"
export DISPLAY=:0 XAUTHORITY=/run/user/1000/gdm/Xauthority
export E2E_RUN_ID=$ID E2E_OUT=$OUT E2E_APP_DIR=$APP
FF=
stop_recorder() {
    [ -n "$FF" ] || return 0
    kill -INT "$FF" 2>/dev/null
    for _ in $(seq 1 60); do kill -0 "$FF" 2>/dev/null || break; sleep 0.25; done
    kill -KILL "$FF" 2>/dev/null
    FF=
}
cleanup() { stop_recorder; sweep "$ID"; }
trap cleanup EXIT
trap 'exit 143' HUP TERM INT

ffmpeg -nostdin -loglevel info -y -f x11grab -framerate 10 -video_size 1280x800 -i "$DISPLAY" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p -stats_period 0.1 -progress "$OUT/ffprogress.txt" \
    "$OUT/raw.mp4" > "$OUT/ffmpeg.log" 2>&1 &
FF=$!
for _ in $(seq 1 200); do
    grep -qE '^frame=[1-9]' "$OUT/ffprogress.txt" 2>/dev/null && break
    kill -0 "$FF" 2>/dev/null || { echo "recorder died" >&2; exit 90; }
    sleep 0.05
done
grep -qE '^frame=[1-9]' "$OUT/ffprogress.txt" 2>/dev/null || { echo "recorder never produced a frame" >&2; exit 91; }
T0=$(date +%s.%N)
export E2E_T0=$T0

timeout -k 5 "$LIMIT" bash "$RUN/scenario/drive.linux.sh" > "$OUT/driver.log" 2>&1
RC=$?
sleep 1.5            # keep the last state on camera
T1=$(date +%s.%N)
stop_recorder
sweep "$ID"
echo "$RC" > "$OUT/driver.rc"
awk -v a="$T0" -v b="$T1" 'BEGIN{printf "%.2f\n", b-a}' > "$OUT/seconds"
exit 0
'''

BUILD_SH = r'''#!/bin/bash
# build.sh <src-dir> <target-dir> <profile>
set -eu
src=$1 target=$2 profile=$3
source "$HOME/.cargo/env"
cd "$src"
# chatd's build.rs says rerun-if-changed=telegram.env, a path cargo reads from crates/chatd. While
# that file is missing cargo reruns the script and relinks chatd (about 3 minutes) on every build,
# so make it exist. Nothing reads it there (the script's own lookup is at the workspace root), so
# the binary is the same one the file's absence gives.
[ -e crates/chatd/telegram.env ] || : > crates/chatd/telegram.env
export CARGO_TARGET_DIR=$target
flag=--release; dir=release
[ "$profile" = dev ] && { flag=; dir=debug; }
# Stable: nightly 2026-10-31 hits a query cycle in zvariant_utils (E0391).
cargo +stable build $flag -p tinyclient --features gui -p chatd -p mediad
sha256sum "$target/$dir/tinyclient" | cut -d' ' -f1 > "$target/$dir/tinyclient.sha256"
echo "built: $target/$dir"
'''


def ssh(host, remote, stdin=None, timeout=None):
    return subprocess.run(SSH + [host, remote], stdin=stdin, capture_output=True, text=True, timeout=timeout)


def make_tar(path, scenario_dir, script):
    """The scenario's folder as `scenario/`, plus the session script."""
    def modes(info):
        info.mode = 0o755 if info.isdir() or info.name.endswith((".sh", ".py")) else 0o644
        return info
    with tarfile.open(path, "w") as t:
        t.add(scenario_dir, arcname="scenario", filter=modes)
        info = tarfile.TarInfo("lab-run.sh")
        info.size = len(script)
        info.mode = 0o755
        info.mtime = int(time.time())
        t.addfile(info, io.BytesIO(script))


def run(ctx):
    host = ctx.host
    target = host["ssh"]
    log = ctx.log
    out = ctx.out_dir
    os.makedirs(out, exist_ok=True)
    profile = host.get("profile", "release")
    dirname = "release" if profile == "release" else "debug"
    run_id = "%s-%s-%d" % (ctx.label, ctx.app_sha[:7], int(time.time()))
    started = False

    home = ssh(target, 'printf %s "$HOME"').stdout.strip()
    if not home:
        return common.Outcome(passed=False, note="cannot reach %s over ssh" % target)
    work = host["work"].replace("~", home, 1)
    src = "%s/src/%s" % (work, ctx.app_sha[:12])
    tgt = "%s/target" % work
    run_dir = "%s/runs/%s" % (work, run_id)
    bin_dir = "%s/%s" % (tgt, dirname)
    logs = []
    local_build_log = os.path.join(out, "build.log")

    def outcome(passed, note, **kw):
        return common.Outcome(passed=passed, note=note, logs=[p for p in logs if os.path.isfile(p)], **kw)

    try:
        # 1. source, once per SHA; old ones beyond KEEP_SOURCES go.
        have = ssh(target, "test -f %s/.extracted && echo yes" % shlex.quote(src)).stdout.strip() == "yes"
        if have:
            log("source %s already on %s" % (ctx.app_sha[:12], target))
        else:
            log("sending source (%d MB)" % (os.path.getsize(ctx.source_tar) // 1048576))
            with open(ctx.source_tar, "rb") as f:
                r = ssh(target, "rm -rf {s} && mkdir -p {s} && tar -x -C {s} && touch {s}/.extracted".format(s=shlex.quote(src)), stdin=f, timeout=600)
            if r.returncode != 0:
                return outcome(False, "could not extract the source on the host: " + r.stderr[-300:])
        ssh(target, "cd %s/src && ls -1t | tail -n +%d | while read d; do rm -rf -- \"$d\"; done" % (shlex.quote(work), KEEP_SOURCES + 1))

        # 2. build
        t = time.time()
        log("building (%s) into %s" % (profile, tgt))
        r = subprocess.run(SSH + [target, "bash -s -- %s %s %s" % (shlex.quote(src), shlex.quote(tgt), profile)],
                           input=BUILD_SH.encode(), capture_output=True, timeout=host.get("build_timeout", 3600))
        r.stdout, r.stderr = r.stdout.decode("utf-8", "replace"), r.stderr.decode("utf-8", "replace")
        with open(local_build_log, "w", encoding="utf-8") as f:
            f.write(r.stdout + "\n" + r.stderr)
        logs.append(local_build_log)
        built = time.time() - t
        log("build %s in %.0fs" % ("ok" if r.returncode == 0 else "FAILED (%d)" % r.returncode, built))
        if r.returncode != 0:
            tail = [l for l in r.stderr.strip().splitlines() if l.strip()][-3:]
            return outcome(False, "build failed after %.0fs: %s" % (built, " | ".join(tail)))
        digest = ssh(target, "cat %s/tinyclient.sha256" % shlex.quote(bin_dir)).stdout.strip()

        # 3. scenario + session script
        tar = os.path.join(out, "scenario-upload.tar")
        make_tar(tar, ctx.scenario_dir, SESSION_SH.encode())
        started = True
        with open(tar, "rb") as f:
            r = ssh(target, "mkdir -p {r} && tar -x -C {r} && mkdir -p {w}/bin && cp {r}/lab-run.sh {w}/bin/lab-run.sh".format(
                r=shlex.quote(run_dir), w=shlex.quote(work)), stdin=f, timeout=120)
        os.remove(tar)
        if r.returncode != 0:
            return outcome(False, "could not copy the scenario to the host: " + r.stderr[-300:], binary_sha256=digest)

        # 4. the recorded run
        limit = int(host.get("driver_timeout", 300))
        env = "E2E_SABOTAGE=%s" % shlex.quote(os.environ.get("E2E_SABOTAGE", ""))
        log("recording and driving")
        r = ssh(target, "%s bash %s/bin/lab-run.sh session %s %s %d" % (env, shlex.quote(work), shlex.quote(run_dir), shlex.quote(bin_dir), limit),
                timeout=limit + 120)
        if r.stderr.strip():
            log("session: " + r.stderr.strip()[-300:])

        # 5. bring everything back
        pulled = subprocess.run(SSH + [target, "tar -c -C %s out" % shlex.quote(run_dir)], capture_output=True, timeout=300)
        if pulled.returncode == 0:
            with tarfile.open(fileobj=io.BytesIO(pulled.stdout)) as t_in:
                t_in.extractall(out, filter="data")
        got = os.path.join(out, "out")

        def local(name):
            p = os.path.join(got, name)
            return p if os.path.isfile(p) else ""

        # the driver copies the app's own logs into out/
        for name in ("driver.log", "ffmpeg.log", "tinyclient.log", "chatd.log"):
            logs.append(os.path.join(got, name))
        rc_text = open(local("driver.rc")).read().strip() if local("driver.rc") else ""
        seconds = float(open(local("seconds")).read().strip()) if local("seconds") else 0.0
        steps = local("steps.jsonl")
        failed = [s["step"] for s in common.read_steps(steps) if not s.get("ok", True)]
        note = "driver exit %s%s; build %.0fs" % (rc_text or "?", ("; failed: " + ", ".join(failed)) if failed else "", built)
        if not local("raw.mp4"):
            note += "; no recording"
        return outcome(rc_text == "0", note, recording=local("raw.mp4"), steps=steps, seconds=seconds, binary_sha256=digest)
    finally:
        # Kill by marker, then drop the run folder. Never by name.
        if started:
            try:
                ssh(target, "bash %s/bin/lab-run.sh cleanup %s; rm -rf -- %s" % (shlex.quote(work), shlex.quote(run_id), shlex.quote(run_dir)), timeout=60)
            except Exception as err:  # the run's result stands; say the cleanup failed
                log("cleanup failed: %s" % err)
