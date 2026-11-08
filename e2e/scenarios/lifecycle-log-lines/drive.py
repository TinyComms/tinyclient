#!/usr/bin/env python3
"""The window's lifecycle lines are on disk while the app runs.

Starts this build's tinyclient with an isolated TINYCLIENT_DIR and reads logs/tinyclient.log
while the process is still running. The fixed build writes "tinyclient: window open" and
flushes it at once, so the line is on disk within seconds. A build from before the fix has no
such line, and buffered what it did write, so this driver fails there.

On Windows the driver then closes the window the way the title bar's X does (WM_CLOSE) and
expects "window should close" and "quit asked" in the file after the process has gone. On Linux
and macOS the driver ends the process itself; the open line is the proof there.

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
    name = "tinyclient.exe" if sys.platform == "win32" else "tinyclient"
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "tinyclient")
    return os.path.join(app_dir, name)


def read_log(path):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            return handle.read()
    except OSError:
        return ""


def wait_for(path, needle, seconds, proc):
    """True once `needle` is in the file, while `proc` (if given) is still running."""
    deadline = time.time() + seconds
    while time.time() < deadline:
        if needle in read_log(path):
            return True
        if proc is not None and proc.poll() is not None:
            return False
        time.sleep(0.5)
    return needle in read_log(path)


def close_window_windows(pid):
    """Posts WM_CLOSE to the process's visible top-level window, as the title bar's X does."""
    import ctypes
    from ctypes import wintypes

    user32 = ctypes.windll.user32
    found = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, wintypes.HWND, wintypes.LPARAM)
    def each(hwnd, _):
        owner = wintypes.DWORD(0)
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd):
            found.append(hwnd)
        return True

    user32.EnumWindows(each, 0)
    if not found:
        return False
    WM_CLOSE = 0x0010
    for hwnd in found:
        user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    return True


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
        step("tinyclient is in E2E_APP_DIR", False)
        return 1

    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    log = os.path.join(data, "logs", "tinyclient.log")
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "app.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "app.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        try:
            step("the window open line is on disk while the app runs", wait_for(log, "tinyclient: window open", 45, proc))
            if sys.platform == "win32":
                time.sleep(2)
                step("the window takes a close from its title bar", close_window_windows(proc.pid))
                try:
                    proc.wait(timeout=20)
                    gone = True
                except subprocess.TimeoutExpired:
                    gone = False
                step("the app exits after the window closes", gone)
                text = read_log(log)
                step("the close left its reason on disk: window should close", "tinyclient: window should close" in text)
                step("the close left its reason on disk: quit asked", "tinyclient: quit asked" in text)
        finally:
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
