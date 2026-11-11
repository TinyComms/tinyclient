#!/usr/bin/env python3
"""Enhanced noise keeps the band above its 7.2 kHz cut, joined at the path's lag.

Starts this build's mediad with TINYCLIENT_PROVE=high-band. That build exits 0
only when the high band rejoins the reconstruction at the path's lag (an
impulse's high-passed response peaks at impulse + RECON_DELAY, and the 10 kHz
sparkle reaches the send at exactly the isolation's own gain, whatever the
model decides about the voice under it). A build from before the rejoin ignores
the variable and opens the audio engine, so this driver fails there; the
intermediate build that rejoined the high band a frame late fails the lag.

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


def mediad_bin(app_dir):
    name = "mediad.exe" if sys.platform == "win32" else "mediad"
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "mediad")
    return os.path.join(app_dir, name)


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    binary = mediad_bin(APP)
    if not os.path.isfile(binary):
        step("mediad is in E2E_APP_DIR", False)
        return 1

    env = os.environ.copy()
    env["TINYCLIENT_PROVE"] = "high-band"
    kwargs = {"env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "app.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "app.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=45)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    step("the high band survives Enhanced and joins at the path's lag", code == 0)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
