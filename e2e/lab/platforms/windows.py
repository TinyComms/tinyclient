"""The Windows lab: builds the real app on `winbuild`, films its screen while a driver uses it.

winbuild is a VM on the Unraid host with an interactive console session (builder, session 1).
An ssh command does not run in that session, so a window opened from it is invisible and no
recorder can see it. Everything that needs the screen (the recorder, the app, the driver) is
therefore started by a one-shot scheduled task made with `schtasks /IT /RU builder /RL LIMITED`:
it runs in the console session as the logged-in user, needs no password and no UAC, and is
removed when the run ends. The build runs over plain ssh, which is enough for cargo.

Host side lives in platforms/windows-host/: tools.ps1 (portable ffmpeg and protoc under
C:\\lab\\tools), build.ps1, run-session.ps1 (the scheduled task) and session.ps1 (the recorder
and the driver, inside the session). Layout under C:\\lab: target (cargo, shared by every SHA),
src\\<sha>, bin\\<sha>, runs\\<sha>-<label> (what one run uses and leaves), logs, in (uploads).
"""
import io
import json
import os
import re
import subprocess
import tarfile
import time

import common

HERE = os.path.dirname(os.path.abspath(__file__))
HOST_DIR = os.path.join(HERE, "windows-host")
SCRIPTS = ("tools.ps1", "build.ps1", "run-session.ps1", "session.ps1")
BUILD_TIMEOUT = 3 * 3600   # a cold build of the whole dependency tree
DRIVER_TIMEOUT = 300
NOISE = re.compile(r"^\*\* |^Warning: Permanently added")


def _clean(text):
    return "\n".join(line for line in text.splitlines() if not NOISE.match(line))


def ssh(host, command, timeout=120, check=True):
    """One ssh command (plain `ssh <alias>`; the remote default shell is PowerShell)."""
    done = subprocess.run(["ssh", host, command], capture_output=True, text=True, timeout=timeout)
    done.stdout, done.stderr = _clean(done.stdout), _clean(done.stderr)
    if check and done.returncode != 0:
        raise RuntimeError("ssh %s failed (%d): %s" % (command[:90], done.returncode, (done.stdout + done.stderr)[-1500:]))
    return done


def scp_to(host, local, remote):
    done = subprocess.run(["scp", "-q", local, "%s:%s" % (host, remote.replace("\\", "/"))], capture_output=True, text=True, timeout=600)
    if done.returncode != 0:
        raise RuntimeError("scp to %s failed: %s" % (remote, _clean(done.stderr)[-500:]))


def scp_from(host, remote, local):
    done = subprocess.run(["scp", "-q", "%s:%s" % (host, remote.replace("\\", "/")), local], capture_output=True, text=True, timeout=600)
    if done.returncode != 0:
        raise RuntimeError("scp from %s failed: %s" % (remote, _clean(done.stderr)[-500:]))


def ps(host, script, args="", **kw):
    return ssh(host, "powershell -NoProfile -ExecutionPolicy Bypass -File %s %s" % (script, args), **kw)


def marker(text, key):
    found = re.findall(r"^%s=(.*)$" % re.escape(key), text, re.M)
    return found[-1].strip() if found else ""


def run(ctx):
    host = ctx.host["ssh"]
    work = ctx.host["work"].rstrip("\\")
    log = ctx.log
    sha = ctx.app_sha[:12]
    rid = "%s-%s" % (sha, ctx.label)
    started = time.time()
    sabotage = os.environ.get("E2E_SABOTAGE") == "1"
    os.makedirs(ctx.out_dir, exist_ok=True)
    notes = []
    try:
        # 1. One upload with the host scripts, the scenario's files and this run's settings; then the
        #    tools (ffmpeg, protoc: no administrator; already there after the first run).
        def w(*parts):
            return "\\".join((work,) + parts)
        tools_ffmpeg = w("tools", "ffmpeg", "bin", "ffmpeg.exe")
        cfg = {"ffmpeg": tools_ffmpeg, "app_dir": w("runs", rid, "app"), "src_dir": w("src", sha),
               "driver": w("runs", rid, "scenario", "drive.windows.ps1"), "driver_timeout": DRIVER_TIMEOUT, "sabotage": sabotage}
        bundle = os.path.join(ctx.out_dir, "bundle.tar")
        with tarfile.open(bundle, "w") as t:
            for name in SCRIPTS:
                t.add(os.path.join(HOST_DIR, name), arcname="host/" + name)
            t.add(ctx.scenario_dir, arcname="in/scenario-" + rid)
            data = json.dumps(cfg).encode()
            entry = tarfile.TarInfo("in/run-%s.json" % rid)
            entry.size = len(data)
            t.addfile(entry, io.BytesIO(data))
        ssh(host, "New-Item -ItemType Directory -Force %s, %s, %s | Out-Null" % (w("in"), w("logs"), w("runs")))
        scp_to(host, bundle, w("in", "bundle-%s.tar" % rid))
        os.remove(bundle)
        step1 = ssh(host, "Remove-Item -Recurse -Force %s -ErrorAction SilentlyContinue; tar.exe -xf %s -C %s; Remove-Item %s; "
                    "powershell -NoProfile -ExecutionPolicy Bypass -File %s -Work %s; "
                    "if (Test-Path %s) { 'BUILT=yes' } else { 'BUILT=no' }" % (
                        w("in", "scenario-" + rid), w("in", "bundle-%s.tar" % rid), work, w("in", "bundle-%s.tar" % rid),
                        w("host", "tools.ps1"), work, w("bin", sha, "tinyclient.exe")), timeout=900)
        ffmpeg = marker(step1.stdout, "ffmpeg")
        if not ffmpeg or ffmpeg.lower() != tools_ffmpeg.lower():
            raise RuntimeError("tools.ps1 did not report ffmpeg: " + step1.stdout[-400:])
        log("tools: " + " | ".join(l for l in step1.stdout.splitlines() if l.startswith(("installed", "ffmpeg version", "libprotoc"))))

        # 2. The source, built once per SHA into the shared cargo target.
        have = "built" if marker(step1.stdout, "BUILT") == "yes" else "missing"
        if have != "built":
            tar_remote = w("in", "src-%s.tar" % sha)
            scp_to(host, ctx.source_tar, tar_remote)
            log("building %s on %s (release, gui; cargo target shared)" % (sha, host))
        else:
            tar_remote = w("in", "none.tar")
            log("binaries for %s are already built" % sha)
        t_build = time.time()
        built = ps(host, w("host", "build.ps1"), "-Tar %s -Id %s -Work %s" % (tar_remote, sha, work), timeout=BUILD_TIMEOUT, check=False)
        build_log = os.path.join(ctx.out_dir, "build.log")
        with open(build_log, "w", encoding="utf-8") as f:
            f.write(built.stdout + built.stderr)
        logs = [build_log]
        if built.returncode != 0:
            last = built.stdout.strip().splitlines()[-1] if built.stdout.strip() else "no output"
            return common.Outcome(False, logs=logs, note="the build failed on %s: %s" % (host, last))
        binary_sha = marker(built.stdout, "TINYCLIENT_SHA256")
        log("build: %s (%.0fs), tinyclient.exe sha256 %s" % ("reused" if have == "built" else "done", time.time() - t_build, binary_sha[:12]))

        # 3. Recorder, app and driver in the console session; wait for the one-shot task to finish.
        session = ps(host, w("host", "run-session.ps1"), "-Id %s -Sha %s -Work %s -Timeout %d" % (rid, sha, work, DRIVER_TIMEOUT + 120),
                     timeout=DRIVER_TIMEOUT + 300, check=False)
        raw = marker(session.stdout, "SESSION")
        if not raw or raw == "timeout":
            notes.append("the session did not finish: " + (session.stdout + session.stderr)[-300:].strip())
            raw = "{}"
        info = json.loads(raw)

        # 4. Bring it all back (run-session.ps1 left out.tar in the run's folder).
        out_tar = w("runs", rid, "out.tar")
        local_tar = os.path.join(ctx.out_dir, "out.tar")
        scp_from(host, out_tar, local_tar)
        with tarfile.open(local_tar) as t:
            t.extractall(ctx.out_dir, filter="data")
        os.remove(local_tar)
        for name in sorted(os.listdir(ctx.out_dir)):
            if name.endswith((".log", ".txt", ".json", ".png")):
                logs.append(os.path.join(ctx.out_dir, name))
        data_logs = os.path.join(ctx.out_dir, "logs")
        if os.path.isdir(data_logs):
            logs += [os.path.join(data_logs, n) for n in sorted(os.listdir(data_logs))]
        recording = os.path.join(ctx.out_dir, "recording.mp4")
        steps = os.path.join(ctx.out_dir, "steps.jsonl")
        driver_exit = info.get("driver_exit")
        if info.get("note"):
            notes.append(info["note"].strip())
        if driver_exit not in (0, None):
            notes.append("driver exited %s" % driver_exit)
        passed = driver_exit == 0 and bool(info.get("recorder_started")) and os.path.isfile(steps)
        return common.Outcome(passed=passed, recording=recording if os.path.isfile(recording) else "",
                              steps=steps if os.path.isfile(steps) else "", seconds=float(info.get("seconds") or 0),
                              logs=logs, binary_sha256=binary_sha, note="; ".join(notes))
    except BaseException:
        # run-session.ps1 removes its own task; this is for an adapter that died while it ran.
        try:
            ssh(host, "schtasks /delete /tn lab-%s /f 2>$null | Out-Null; exit 0" % rid, timeout=30, check=False)
        except Exception:
            pass
        raise
    finally:
        log("windows run took %.0fs" % (time.time() - started))
