#!/usr/bin/env python3
"""Run this build's real long-conversation sealed Cache restart regression in chatd.

The proof writes 20,001 sealed records through the production Cache and
requires every one to remain readable after reopening. The base lacks this mode and
cannot emit its success marker. This is a daemon check, not native GUI proof.
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
    env["TINYCLIENT_PROVE"] = "cache-history-limit"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=120)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    with open(os.path.join(OUT, "chatd.err.log"), encoding="utf-8") as handle:
        checked_cache = "prove-cache-history-limit:0" in handle.read()
    step("encrypted history beyond 20,000 messages survives restart", code == 0 and checked_cache)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
