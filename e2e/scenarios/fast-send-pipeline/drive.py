#!/usr/bin/env python3
"""Fast send pipeline proof.

Starts this build's chatd with TINYCLIENT_PROVE=fast-send-pipeline. That build exits 0
only when:
  1. Real Slack adapter completes Command::Send in < 1.5s while Backfill stalls on
     conversations.history for 4s.
  2. serve_commands wakes out of a 2s pace sleep and runs Command::Send ahead of queued
     Warm/Previews in < 200ms.
  3. Store::messages(Page::After { id: pending_client_id }) resolves the replacement
     settled message after ctx.sent instead of returning empty [].
  4. Store::fail_pending_excluding keeps an active upload Pending past 60s while marking
     an inactive >60s pending row Failed.

A build from before the fix ignores the variable and keeps serving, so this driver fails there.
This is a daemon/component proof, not native GUI certification.
Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR.
"""

import os
import subprocess
import sys
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")

FAILED = False
STEPS = ""


def now():
    return time.time() - T0


def step(words, ok):
    global FAILED
    if not ok:
        FAILED = True
    line = '{"t": %s, "step": "%s", "ok": %s}' % (round(now(), 1), words.replace('"', "'"), "true" if ok else "false")
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def chatd_bin(app_dir):
    if sys.platform == "win32":
        return os.path.join(app_dir, "chatd.exe")
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "chatd")
    return os.path.join(app_dir, "chatd")


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    binary = chatd_bin(APP)
    if not os.path.isfile(binary):
        step("chatd is in E2E_APP_DIR", False)
        return 1

    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_PROVE"] = "fast-send-pipeline"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    with open(os.path.join(OUT, "chatd.err.log"), encoding="utf-8") as handle:
        marked = "prove-fast-send-pipeline:0" in handle.read()
    step("fast-send-pipeline proves non-blocking send, pace wake, Page::After anchor, and active upload exemption", code == 0 and marked)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
