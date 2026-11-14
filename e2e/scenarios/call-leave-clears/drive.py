#!/usr/bin/env python3
"""Leaving a call clears it from the shell, whatever its mediad says while shutting down.

Starts this build's chatd with TINYCLIENT_PROVE=call-leave-clears. That build puts a joined
call through its own hub the way the shell reads it: the hang-up tells the shell there is no
call, and mediad's last word (who stopped speaking) does not tell it of the call again. It exits
0 only when that holds. A build from before the fix ignores the variable and serves forever, so
this driver fails there.

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


def app_bin(app_dir):
    name = "chatd.exe" if sys.platform == "win32" else "chatd"
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "chatd")
    return os.path.join(app_dir, name)


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    binary = app_bin(APP)
    if not os.path.isfile(binary):
        step("chatd is in E2E_APP_DIR", False)
        return 1

    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_PROVE"] = "call-leave-clears"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    step("leaving a call clears it and mediad's last word does not put it back", code == 0)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
