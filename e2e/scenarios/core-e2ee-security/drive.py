#!/usr/bin/env python3
"""Core E2EE, reaction reload, stale-row upgrade, and bounded event worker proof.

Starts this build's chatd with TINYCLIENT_PROVE=core-e2ee-security. That build drives two
in-process MLS devices against a localhost fake tinyserver and verifies:
  1. Unicode and custom-emoji (:party_parrot:) sealed reactions survive history and thread reloads.
  2. A stale local plaintext row hitting 400 e2ee_required upgrades to sealed and retries via MLS.
  3. Live e2ee.reaction echoes land through the bounded per-account E2EE worker channel.

A build from before this fix ignores TINYCLIENT_PROVE=core-e2ee-security and keeps serving until
timeout, so the base run fails and the branch run passes.

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
    env["TINYCLIENT_PROVE"] = "core-e2ee-security"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    err_path = os.path.join(OUT, "chatd.err.log")
    with open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as out, open(err_path, "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=90)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    stderr_text = ""
    if os.path.isfile(err_path):
        with open(err_path, "r", encoding="utf-8", errors="replace") as handle:
            stderr_text = handle.read()
    step(
        "core E2EE reaction reload, e2ee_required retry, and bounded worker proof pass",
        code == 0 and "prove-core-e2ee-security:0" in stderr_text,
    )
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
