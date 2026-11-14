#!/usr/bin/env python3
"""A voice held up in mid-word and then here in a lump is paced, and the stall is measured.

Starts this build's mediad with TINYCLIENT_PROVE=voice-stall. That build plays three seconds
of speech through a receive stream on a clean link: nothing lost, eight packets in mid-word
held up 160 ms and then arriving together. It exits 0 only when no two frames in a row were
squeezed, no more stayed queued than the buffer now keeps, nothing was taken for lost, and
the stall was measured once (how long something stood in, the lump, and what the loop saw
of its sockets), saying
"voice-stall: proved" on stderr. A build from before the fix ignores the variable and never
says it, so this driver fails there.

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
    env["TINYCLIENT_PROVE"] = "voice-stall"
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
    # Any mediad exits 0 once its input closes, so the exit code alone proves nothing: this
    # build says so when the proof passed.
    with open(os.path.join(OUT, "app.err.log"), encoding="utf-8", errors="replace") as err:
        proved = "voice-stall: proved" in err.read()
    step("a stalled voice is paced out of its lump and the stall is measured once", code == 0 and proved)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
