#!/usr/bin/env python3
"""The shipped mediad downloads Cisco's OpenH264 into its codec folder and loads it.

Starts this build's mediad with TINYCLIENT_PROVE=openh264-download and a codec folder of
its own that is empty: an isolated LOCALAPPDATA on Windows, HOME elsewhere. That build
fetches the library from ciscobinary.openh264.org the way a call does and exits 0 only
when the file loads, saying "openh264-download: proved" on stderr.

The exit code alone proves nothing: any mediad that is asked for no call exits 0. So the
step holds only when mediad says the words and the library is on disk in that folder. A
build from before the change ignores the variable: it exits 0 at once, says nothing, and
the folder stays empty, which is the defect (#967: the download's request had no TLS
backend). This driver fails there.

Needs the network: the host must reach ciscobinary.openh264.org over HTTPS.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR.
"""

import os
import subprocess
import sys
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")
PROVED = "openh264-download: proved"
# Three tries of 20 seconds, with pauses of 1 and 4 seconds between them, then the start and
# the load.
WAIT = 150

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


def tail(path, lines):
    """The last lines of a text file, for this driver's own log. Nothing when it is not there."""
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return handle.read().rstrip().splitlines()[-lines:]
    except OSError:
        return []


def folder(where):
    """What is in `where` just now, each as (name, size). Nothing when the folder is not there."""
    try:
        return sorted((entry.name, entry.stat().st_size if entry.is_file() else -1) for entry in os.scandir(where))
    except OSError:
        return []


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

    # A world of this run's own: the log goes under TINYCLIENT_DIR, and the codec is kept
    # where the shipped process keeps it, in an isolated %LOCALAPPDATA%\tinyclient on Windows
    # and ~/.cache/tinyclient elsewhere.
    world = os.path.join(OUT, "openh264-download")
    data = os.path.join(world, "data")
    home = os.path.join(world, "home")
    os.makedirs(data, exist_ok=True)
    os.makedirs(home, exist_ok=True)
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    env["TINYCLIENT_PROVE"] = "openh264-download"
    if sys.platform == "win32":
        env["LOCALAPPDATA"] = home
        codec = os.path.join(home, "tinyclient")
    else:
        # The codec folder reads LOCALAPPDATA before HOME, so a stray one must not be inherited.
        env.pop("LOCALAPPDATA", None)
        env["HOME"] = home
        codec = os.path.join(home, ".cache", "tinyclient")
    step("the codec folder is empty before the download", folder(codec) == [])

    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "app.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "app.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            code = proc.wait(timeout=WAIT)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            code = None
    said = tail(os.path.join(OUT, "app.err.log"), 10)
    # The download says precisely what went wrong only in its own log.
    for line in tail(os.path.join(data, "logs", "mediad.log"), 6):
        print("mediad.log: " + line, flush=True)
    for line in said:
        print("stderr: " + line, flush=True)
    # Any mediad exits 0 once its input closes, so the exit code alone proves nothing: this
    # build says so when the library is loaded.
    step("mediad downloads OpenH264 from Cisco and exits 0 having loaded it", code == 0 and PROVED in said)
    kept = folder(codec)
    print("codec folder: %s" % (kept or "empty"), flush=True)
    step("Cisco's library is on disk in the isolated codec folder", len(kept) == 1 and kept[0][1] > 0)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
