"""macOS adapter: builds the real app on the MacBook (`mbp`), films the tinyclient WINDOW only while
the scenario's drive.macos.sh runs, and brings the video and the step log back.

Privacy: the Mac is a personal laptop and the recordings are published. The recorder is
mac/helper/main.swift. It is given the app's process id and asks ScreenCaptureKit for that
process's window, so it has no code path that films the desktop, the menu bar or another app. It
needs a one-time Screen Recording and Accessibility grant for TinyLabHelper.app; without it this
adapter says so and records nothing.

  1. the lab scripts and the source tar go to ~/tiny-lab (the work folder, on the external drive);
  2. build-macos.sh: one cargo build into the shared target (incremental), bundle.sh --no-build;
  3. the scenario's folder goes next to the app; run-macos.sh starts the recorder (E2E_T0 is its
     first clock tick), runs drive.macos.sh, stops the recorder with SIGINT;
  4. out/ comes back into ctx.out_dir and the run folder is removed from the Mac.
"""
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
import _mac  # noqa: E402
import common  # noqa: E402


def _read(name):
    with open(os.path.join(_mac.MAC, name), encoding="utf-8", newline="") as f:
        return f.read().replace("\r\n", "\n")


def run(ctx):
    log, host = ctx.log, ctx.host
    if host.get("work", _mac.WORK) != _mac.WORK:
        raise RuntimeError("the Mac scripts assume the work folder %s; hosts.json says %s" % (_mac.WORK, host.get("work")))
    run_id = "macos-%s-%s" % (ctx.app_sha[:7], ctx.label)
    started = time.time()

    _mac.ship_scripts(host)
    remote_tar = "/tmp/lab-src-%s.tar" % ctx.app_sha[:7]
    _mac.put(host, ctx.source_tar, remote_tar)

    log("building the macOS app on the Mac (release profile: gui + chatd + mediad)")
    t = time.time()
    code, text = _mac.remote(host, _read("build-macos.sh"), {"SRC_TAR": remote_tar, "RUN": run_id}, log, timeout=int(host.get("build_timeout", 3600)))
    build_log = os.path.join(ctx.out_dir, "build.log")
    with open(build_log, "w", encoding="utf-8") as f:
        f.write(text)
    if code != 0:
        return common.Outcome(False, logs=[build_log], note="the macOS build failed (exit %d); see build.log" % code)
    binary = _mac.value(text, "BINARY_SHA256")
    log("build step took %.0f s (cargo and bundling %s s on the Mac)" % (time.time() - t, _mac.value(text, "BUILD_SECONDS")))

    _mac.put_dir(host, ctx.scenario_dir, "/tmp/tiny-lab-runs/%s/scenario" % run_id)
    env = {"RUN": run_id}
    if os.environ.get("E2E_SABOTAGE"):
        env["E2E_SABOTAGE"] = os.environ["E2E_SABOTAGE"]
    code, text = _mac.remote(host, _read("run-macos.sh"), env, log, timeout=int(host.get("driver_timeout", 600)))
    run_log = os.path.join(ctx.out_dir, "run.log")
    with open(run_log, "w", encoding="utf-8") as f:
        f.write(text)
    logs = [build_log, run_log]
    if "RESULT=norecorder" in text:
        _mac.tidy(host, run_id, remote_tar)
        return common.Outcome(False, logs=logs, binary_sha256=binary, note=(
            "nothing was filmed: TinyLabHelper has no Screen Recording grant on the Mac (System Settings > Privacy & Security "
            "> Screen & System Audio Recording; Accessibility too, for keys). " + _mac.value(text, "HELPER_STATUS")))

    out = os.path.join(ctx.out_dir, "mac-out")
    _mac.fetch_dir(host, "/tmp/tiny-lab-runs/%s/out" % run_id, out)
    _mac.tidy(host, run_id, remote_tar)
    for name in sorted(os.listdir(out)):
        if name.endswith((".log", ".txt")):
            logs.append(os.path.join(out, name))
    recording, steps = os.path.join(out, "recording.mp4"), os.path.join(out, "steps.jsonl")
    rc = _mac.value(text, "DRIVER_RC")
    return common.Outcome(
        passed=(rc == "0"), recording=recording if os.path.isfile(recording) else "",
        steps=steps if os.path.isfile(steps) else "", seconds=float(_mac.value(text, "SECONDS") or 0),
        logs=logs, binary_sha256=binary, note="driver exit %s; adapter total %.0f s" % (rc or "?", time.time() - started))
