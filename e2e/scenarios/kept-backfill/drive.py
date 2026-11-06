#!/usr/bin/env python3
"""A history catch-up is kept until the adapter exists.

Starts this build's chatd with TINYCLIENT_PROVE=kept-backfill. That build exits 0 only
when a backfill asked before the adapter exists is kept, and a call leave is not.
A build from before the fix ignores the variable and keeps serving, so this driver fails there.

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
    env["TINYCLIENT_PROVE"] = "kept-backfill"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=45)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    step("a backfill before the adapter exists is kept", code == 0)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
