#!/usr/bin/env python3
"""Closing tinyclient with an update waiting installs it and leaves the app closed.

The driver makes what an installed copy with a finished download looks like, from this build's
own programs and nothing from the network:

  <E2E_OUT>/install    the programs, copied out of the build folder (a copy under Cargo's
                       target/ never replaces itself)
  <TINYCLIENT_DIR>/update/staged   the same programs again, as the download waiting to go in,
                       with a marker so they can be told from the installed ones
  <TINYCLIENT_DIR>/update/state    "ready", as the updater writes it after a download

It then starts the installed copy, waits for its window, and closes it the way a person does: the
title bar's X on Windows and Linux, Command-Q on macOS. No Restart button is pressed.

What it proves: after that close the marked programs are the installed ones, the swap left
`updated <version>` in update/swap-result, and for 12 s afterwards nothing is running from the
install folder. A build from before the fix reopens tinyclient at the end of every swap, so it
fails the last step on every platform, and on macOS and Linux it also leaves no swap-result.

What it does not prove: the download and its checksum (nothing is fetched here: the update check
is pointed at a closed local port so it cannot answer mid-run), and the Restart buttons, which
still reopen the app.

Two stand-ins, both said here because the recording cannot show them. The lab's Windows and Linux
builds have no tinyclient-login, which the updater wants beside the others before it will replace
anything, so a copy of mediad takes its name; the updater only moves that file. And the staged
version is this build's own (`tinyclient --version`), so the Windows swap's "Installed apps"
version line, if that machine has tinyclient installed, is written with a real version.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR, and on macOS E2E_HELPER.
"""

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")
HELPER = os.environ.get("E2E_HELPER", "")

WINDOWS = sys.platform == "win32"
MAC = sys.platform == "darwin"
EXE = ".exe" if WINDOWS else ""
PROGRAMS = ["tinyclient", "chatd", "mediad", "tinyclient-login"]
MARKER = b"\ne2e-staged-update\n"
HIDDEN = {"creationflags": 0x08000000} if WINDOWS else {}

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


def read(path):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            return handle.read()
    except OSError:
        return ""


def wait_until(seconds, check):
    deadline = time.time() + seconds
    while time.time() < deadline:
        if check():
            return True
        time.sleep(0.5)
    return check()


def run(args, seconds=30):
    try:
        return subprocess.run(args, capture_output=True, text=True, timeout=seconds, stdin=subprocess.DEVNULL, **HIDDEN).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def install_root():
    """A folder for the installed copy with no `target` in its path, or the updater leaves it alone."""
    root = os.path.realpath(os.path.join(OUT, "install"))
    if "target" in re.split(r"[\\/]", root):
        root = os.path.realpath(tempfile.mkdtemp(prefix="tiny-e2e-install-"))
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root)
    return root


def lay_out(data):
    """The installed copy and the download waiting for it. Returns (program to start, how to see the swap)."""
    root = install_root()
    staged = os.path.join(data, "update", "staged")
    os.makedirs(staged)
    if MAC:
        app = os.path.join(root, "tinyclient.app")
        shutil.copytree(APP, app, symlinks=True)
        waiting = os.path.join(staged, "tinyclient.app")
        shutil.copytree(APP, waiting, symlinks=True)
        os.makedirs(os.path.join(waiting, "Contents", "Resources"), exist_ok=True)
        with open(os.path.join(waiting, "Contents", "Resources", "e2e-staged-update"), "wb") as handle:
            handle.write(MARKER)
        marked = os.path.join(app, "Contents", "Resources", "e2e-staged-update")
        return os.path.join(app, "Contents", "MacOS", "tinyclient"), root, lambda: os.path.isfile(marked)
    for name in PROGRAMS:
        source = os.path.join(APP, name + EXE)
        if not os.path.isfile(source):
            # The stand-in the docstring names: the lab build has no sign-in helper.
            source = os.path.join(APP, "mediad" + EXE)
        shutil.copy2(source, os.path.join(root, name + EXE))
        shutil.copy2(source, os.path.join(staged, name + EXE))
    with open(os.path.join(staged, "tinyclient-login" + EXE), "ab") as handle:
        handle.write(MARKER)
    marked = os.path.join(root, "tinyclient-login" + EXE)

    def swapped():
        try:
            with open(marked, "rb") as handle:
                handle.seek(-len(MARKER), os.SEEK_END)
                return handle.read() == MARKER
        except OSError:
            return False

    return os.path.join(root, "tinyclient" + EXE), root, swapped


def build_version(binary):
    found = re.search(r"tinyclient (\S+)", run([binary, "--version"], 20))
    return found.group(1) if found else "0.0.0"


def running_from(root):
    """Pids of whatever is running out of the install folder."""
    pids = []
    if WINDOWS:
        script = "Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith('%s\\', 'OrdinalIgnoreCase') } | ForEach-Object { $_.Id }" % root.replace("'", "''")
        text = run(["powershell", "-NoProfile", "-Command", script], 30)
        return [int(line) for line in text.split() if line.isdigit()]
    for line in run(["ps", "-axo", "pid=,args="], 15).splitlines():
        pid, _, args = line.strip().partition(" ")
        if pid.isdigit() and args.strip().startswith(root + os.sep):
            pids.append(int(pid))
    return pids


def stop_all(root):
    for pid in running_from(root):
        if WINDOWS:
            run(["taskkill", "/F", "/PID", str(pid)], 15)
        else:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass


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
    for hwnd in found:
        user32.PostMessageW(hwnd, 0x0010, 0, 0)
    return bool(found)


def close_window_linux(pid):
    """Asks the window manager to close the process's window, as the title bar's X does."""
    for line in run(["wmctrl", "-lp"], 10).splitlines():
        fields = line.split()
        if len(fields) > 2 and fields[2] == str(pid):
            return subprocess.call(["wmctrl", "-i", "-c", fields[0]]) == 0
    return False


def quit_mac(pid):
    """Command-Q, sent to this process only by the lab's helper."""
    if not HELPER:
        return False
    said = os.path.join(OUT, "helper.txt")
    subprocess.call(["open", "-W", "-n", "--stdout", said, "--stderr", said, "-a", HELPER, "--args", "key", "--pid", str(pid), "--combo", "cmd-q"])
    return read(said).startswith("ok")


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    built = os.path.join(APP, "Contents", "MacOS", "tinyclient") if MAC else os.path.join(APP, "tinyclient" + EXE)
    if not os.path.isfile(built):
        step("tinyclient is in E2E_APP_DIR", False)
        return 1

    data = os.path.join(OUT, "data")
    shutil.rmtree(data, ignore_errors=True)
    os.makedirs(data)
    binary, root, swapped = lay_out(data)
    version = build_version(built)
    update = os.path.join(data, "update")
    with open(os.path.join(update, "state"), "w", encoding="utf-8", newline="\n") as handle:
        handle.write("checked\t%d\nshown\tready\nversion\t%s\npage\thttps://github.com/TinyComms/tinyclient/releases/latest\n" % (time.time(), version))
    step("an installed copy has version %s downloaded and waiting" % version, os.path.isfile(binary) and not swapped())

    log = os.path.join(data, "logs", "tinyclient.log")
    result = os.path.join(update, "swap-result")
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    # The check a few seconds after launch gets no answer, so the invented download stays as it is.
    for name in ("HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"):
        env[name] = "http://127.0.0.1:9"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    kwargs.update(HIDDEN)
    try:
        with open(os.path.join(OUT, "app.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "app.err.log"), "w", encoding="utf-8") as err:
            proc = subprocess.Popen([binary], stdout=out, stderr=err, **kwargs)
            with open(os.path.join(OUT, "app.pid"), "w", encoding="utf-8") as handle:
                handle.write("%d\n" % proc.pid)
            opened = wait_until(45, lambda: "tinyclient: window open" in read(log) or proc.poll() is not None)
            step("the window is open", opened and proc.poll() is None)
            time.sleep(3)
            if WINDOWS:
                asked = close_window_windows(proc.pid)
            elif MAC:
                asked = quit_mac(proc.pid)
            else:
                asked = wait_until(15, lambda: close_window_linux(proc.pid))
            step("the window is closed, with no Restart pressed", asked)
            try:
                proc.wait(timeout=20)
                gone = True
            except subprocess.TimeoutExpired:
                gone = False
                proc.kill()
                proc.wait(timeout=10)
            step("the app exits", gone)

        # The swap waits for the app and its helpers to end, so it lands some seconds later.
        wait_until(60, lambda: swapped() or os.path.isfile(result))
        step("the downloaded programs replaced the installed ones", wait_until(5, swapped))
        step("the swap said so: updated %s" % version, wait_until(5, lambda: read(result).strip() == "updated %s" % version))
        reopened = wait_until(12, lambda: bool(running_from(root)))
        step("tinyclient stays closed after the swap", not reopened)
    finally:
        stop_all(root)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
