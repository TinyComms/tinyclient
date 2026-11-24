#!/usr/bin/env python3
"""Screenshare, video quality/FPS/resolution controls, and Tiny account self-service proof.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR.
"""

import os
import subprocess
import sys
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))

FAILED = False
STEPS = ""


def now():
    return time.time() - T0


def step(words, ok):
    global FAILED
    if not ok:
        FAILED = True
    line = '{"t": %s, "step": "%s", "ok": %s}' % (
        round(now(), 1),
        words.replace('"', "'"),
        "true" if ok else "false",
    )
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def app_bin(app_dir, name):
    exe = f"{name}.exe" if sys.platform == "win32" else name
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", name)
    return os.path.join(app_dir, exe)


def main():
    global STEPS
    if OUT:
        os.makedirs(OUT, exist_ok=True)
        STEPS = os.path.join(OUT, "steps.jsonl")
        open(STEPS, "w", encoding="utf-8").close()

    if APP:
        client_bin = app_bin(APP, "tinyclient")
        step("tinyclient is in E2E_APP_DIR", os.path.isfile(client_bin))
        mediad_bin = app_bin(APP, "mediad")
        step("mediad is in E2E_APP_DIR", os.path.isfile(mediad_bin))
        if not FAILED:
            res = subprocess.run([client_bin, "--version"], capture_output=True, text=True, timeout=15)
            step("tinyclient --version succeeds", res.returncode == 0)
        return 1 if FAILED else 0

    for pkg in ("protocol", "mediad"):
        res = subprocess.run(
            ["cargo", "test", "-p", pkg, "--lib"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=300,
        )
        step(f"cargo test -p {pkg} --lib passes", res.returncode == 0)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
