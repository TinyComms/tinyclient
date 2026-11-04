"""iOS adapter: builds the app for the iOS simulator on the MacBook (`mbp`), installs it on the
simulator named in hosts.json, films the simulator's own screen (`simctl io recordVideo`; the
Mac's display is never captured) while the scenario's drive.ios.sh runs, and brings the video and
the step log back.

  1. the lab scripts and the source tar go to ~/tiny-lab (the work folder, on the external drive);
  2. build-ios.sh: scripts/mobile/build-ios.sh --debug --demo --sim-only (Rust, incremental in the
     shared target), xcodegen, xcodebuild for the iOS simulator, then the input runner;
  3. the scenario's folder goes next to the app; run-ios.sh boots the simulator if it is not up,
     installs, starts the recorder (E2E_T0 is the moment it said "Recording started"), runs
     drive.ios.sh, stops the recorder with SIGINT, uninstalls the app and shuts the simulator down
     only if this run booted it;
  4. out/ comes back into ctx.out_dir and the run folder is removed from the Mac.

Why a `demo` build: iOS has no way to pass accounts or a data folder to the app, and the shipped
app has no demo mode. The `demo` feature puts the made-up chatd of `--shot --demo` inside the app,
on from launch, with invented people (crates/tinyclient/src/gui/shot_demo.rs). So the iOS app is
the real window code with a fake core, not the shipped binary; the note says so.
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
    run_id = "ios-%s-%s" % (ctx.app_sha[:7], ctx.label)
    started = time.time()

    _mac.ship_scripts(host)
    remote_tar = "/tmp/lab-src-%s.tar" % ctx.app_sha[:7]
    _mac.put(host, ctx.source_tar, remote_tar)

    log("building the iOS simulator app on the Mac (debug + demo, simulator slice)")
    t = time.time()
    code, text = _mac.remote(host, _read("build-ios.sh"), {"SRC_TAR": remote_tar, "RUN": run_id}, log, timeout=int(host.get("build_timeout", 3600)))
    build_log = os.path.join(ctx.out_dir, "build.log")
    with open(build_log, "w", encoding="utf-8") as f:
        f.write(text)
    if code != 0:
        _mac.tidy(host, run_id, remote_tar)
        return common.Outcome(False, logs=[build_log], note="the iOS build failed (exit %d); see build.log" % code)
    binary = _mac.value(text, "BINARY_SHA256")
    log("build step took %.0f s (%s s on the Mac)" % (time.time() - t, _mac.value(text, "BUILD_SECONDS")))

    _mac.put_dir(host, ctx.scenario_dir, "/tmp/tiny-lab-runs/%s/scenario" % run_id)
    env = {"RUN": run_id, "DEVICE": host.get("device", "iPhone 18 Pro")}
    if os.environ.get("E2E_SABOTAGE"):
        env["E2E_SABOTAGE"] = os.environ["E2E_SABOTAGE"]
    code, text = _mac.remote(host, _read("run-ios.sh"), env, log, timeout=int(host.get("driver_timeout", 600)))
    run_log = os.path.join(ctx.out_dir, "run.log")
    with open(run_log, "w", encoding="utf-8") as f:
        f.write(text)
    logs = [build_log, run_log]
    note = "a demo build (made-up chatd inside the app), not the shipped binary"
    if "RESULT=" in text and not _mac.value(text, "DRIVER_RC"):
        _mac.tidy(host, run_id, remote_tar)
        return common.Outcome(False, logs=logs, binary_sha256=binary, note="the simulator run did not start: " + _mac.value(text, "RESULT"))

    out = os.path.join(ctx.out_dir, "mac-out")
    _mac.fetch_dir(host, "/tmp/tiny-lab-runs/%s/out" % run_id, out)
    _mac.tidy(host, run_id, remote_tar)
    for name in sorted(os.listdir(out)):
        if name.endswith((".log", ".txt", ".png")):
            logs.append(os.path.join(out, name))
    recording, steps = os.path.join(out, "recording.mp4"), os.path.join(out, "steps.jsonl")
    rc = _mac.value(text, "DRIVER_RC")
    return common.Outcome(
        passed=(rc == "0"), recording=recording if os.path.isfile(recording) else "",
        steps=steps if os.path.isfile(steps) else "", seconds=float(_mac.value(text, "SECONDS") or 0),
        logs=logs, binary_sha256=binary, note="%s; driver exit %s; adapter total %.0f s" % (note, rc or "?", time.time() - started))
