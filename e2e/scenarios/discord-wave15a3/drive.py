#!/usr/bin/env python3
"""Discord Wave 15A3 proof: VP8/AV1 codec negotiation, camera simulcast, per-SSRC op 15, and multi-stream watch.

Starts this build's chatd, then mediad, both with TINYCLIENT_PROVE=discord-wave15a3.
Each exits 0 only when its half of the check holds. A build from before the change ignores
the variable and stays up, so this driver fails there.

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


def app_bin(app_dir, name):
    binary = name + ".exe" if sys.platform == "win32" else name
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", binary)
    return os.path.join(app_dir, binary)


def run(binary, log_name):
    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_PROVE"] = "discord-wave15a3"
    env["TINYCLIENT_SHARE_AUDIO"] = "null"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, log_name + ".out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, log_name + ".err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            return proc.wait(timeout=45)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            return None


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    chatd = app_bin(APP, "chatd")
    mediad = app_bin(APP, "mediad")
    if not os.path.isfile(chatd) or not os.path.isfile(mediad):
        step("chatd and mediad are in E2E_APP_DIR", False)
        return 1
    step("discord adapter tracks multiple watched streams independently", run(chatd, "chatd") == 0)
    step("mediad negotiates VP8/AV1 decode, camera simulcast, per-SSRC sink wants, and decodes AV1", run(mediad, "mediad") == 0)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
