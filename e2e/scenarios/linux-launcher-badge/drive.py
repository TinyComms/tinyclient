#!/usr/bin/env python3
"""A Tiny direct message puts 1 on the Linux launcher entry, and reading it clears it.

The badge is one `com.canonical.Unity.LauncherEntry.Update` signal on the session bus, the one
signal Ubuntu Dock, Dash to Dock and KDE's task manager read. `dbus-monitor` listens for it
from before the window opens. The fixture's first list says Bea's direct is unread, so the
window's first status must put count 1, count-visible true on the bus; then the fixture says
the direct was read on another device (`read.updated`), and the badge must clear. On a build
from before the fix no signal is ever sent, so the first badge step fails.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR, E2E_SABOTAGE=1 (nobody signs in, so the badge
step fails there too).
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import time

import fixture

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")
SABOTAGE = os.environ.get("E2E_SABOTAGE") == "1"
ME = fixture.ME
TOKEN = fixture.TOKEN

FAILED = False
STEPS = ""
MONITOR = None
MONITOR_LOG = ""


def now():
    return time.time() - T0


def step(words, ok):
    global FAILED
    if not ok:
        FAILED = True
    line = json.dumps({"t": round(now(), 1), "step": words, "ok": bool(ok)}, ensure_ascii=True)
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def binaries(app_dir):
    if sys.platform == "win32":
        return os.path.join(app_dir, "chatd.exe"), os.path.join(app_dir, "tinyclient.exe")
    return os.path.join(app_dir, "chatd"), os.path.join(app_dir, "tinyclient")


def scratch_dir():
    if sys.platform == "win32":
        path = os.path.join(OUT, "data")
        os.makedirs(path, exist_ok=True)
        return path
    return tempfile.mkdtemp(prefix="tlbadge-")


def session(port):
    return json.dumps({
        "base_url": "http://127.0.0.1:%d" % port,
        "token": TOKEN,
        "user_id": ME,
        "email": "ada@example.com",
        "display_name": "Ada",
        "role": "member",
    })


def child_env(data, port):
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("TINYCLIENT_SESSION_"):
            del env[key]
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    if not SABOTAGE:
        # The label is "Tiny server", so the scratch session name keeps the space.
        env["TINYCLIENT_SESSION_TINY SERVER"] = session(port)
    return env


def popen(args, **kwargs):
    if sys.platform == "win32":
        kwargs["creationflags"] = kwargs.get("creationflags", 0x08000000)
    return subprocess.Popen(args, **kwargs)


def stop(proc):
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()


def wait_until(limit, predicate):
    deadline = time.time() + limit
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def start_monitor(path):
    """Watches the session bus for the one signal the docks read. Started first: a signal
    sent before this runs is a step that fails, not a badge that was missed."""
    global MONITOR, MONITOR_LOG
    MONITOR_LOG = path
    with open(path, "w", encoding="utf-8") as out:
        MONITOR = subprocess.Popen(
            ["dbus-monitor", "type='signal',interface='com.canonical.Unity.LauncherEntry',member='Update'"],
            stdout=out,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
        )
    time.sleep(0.5)
    return MONITOR.poll() is None


def updates():
    """The Update signals so far, oldest first: (entry, count, count_visible)."""
    try:
        with open(MONITOR_LOG, "r", encoding="utf-8", errors="replace") as handle:
            text = handle.read()
    except OSError:
        return []
    found = []
    for block in text.split("interface=com.canonical.Unity.LauncherEntry; member=Update")[1:]:
        entry = re.search(r'string "(application://[^"]+)"', block)
        count = re.search(r'string "count"\s*\n\s*int64 (-?\d+)', block)
        visible = re.search(r'string "count-visible"\s*\n\s*boolean (true|false)', block)
        if entry and count and visible:
            found.append((entry.group(1), int(count.group(1)), visible.group(1) == "true"))
    return found


def latest_badge():
    """The newest signal for this app's entry, or None: (count, count_visible)."""
    for entry, count, visible in reversed(updates()):
        # The release build names the installed desktop file; a debug build its own.
        if entry.startswith("application://dev.tinyclient.app"):
            return count, visible
    return None


def badge_shows(count, visible):
    shown = latest_badge()
    return shown == (count, visible)


def inbox(tinyclient, env, data):
    kwargs = {"cwd": data, "env": env, "capture_output": True, "text": True, "timeout": 30}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    done = subprocess.run([tinyclient, "--text"], **kwargs)
    return done.returncode, done.stdout or ""


def badge_of(text, title):
    """The unread column of one inbox row. None when that title is not listed.

    `--text` prints `"{:<8} {:<24} {:>3}"`. The network label is wider than 8, so the
    title starts after that whole label and the following space. One more space separates
    the title from the 3-column unread.
    """
    prefix = "Tiny server "
    for line in text.splitlines():
        if not line.startswith(prefix):
            continue
        rest = line[len(prefix):]
        if len(rest) < 28 or rest[:24].rstrip() != title:
            continue
        return rest[25:28].strip()
    return None


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    chatd_bin, tiny_bin = binaries(APP)
    if not os.path.isfile(chatd_bin) or not os.path.isfile(tiny_bin):
        step("chatd and tinyclient are in E2E_APP_DIR", False)
        return 1
    if not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        step("the session's bus is reachable", False)
        return 1

    watching = start_monitor(os.path.join(OUT, "dbus-monitor.log"))
    step("dbus-monitor watches com.canonical.Unity.LauncherEntry", watching)
    if not watching:
        return 1

    data = scratch_dir()
    httpd, state = fixture.serve()
    port = httpd.server_address[1]
    env = child_env(data, port)
    chatd = None
    window = None
    try:
        chatd = popen(
            [chatd_bin],
            cwd=data,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8"),
            stderr=open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8"),
        )
        answered = wait_until(45, lambda: inbox(tiny_bin, env, data)[0] == 0)
        step("chatd answers on the scratch directory", answered and chatd.poll() is None)
        if not answered:
            return 1

        code, text = inbox(tiny_bin, env, data)
        unread = badge_of(text, "Bea")
        step("Bea's direct is unread in the inbox", unread == "1")

        if SABOTAGE:
            # Nobody signed in, so there is no unread and no badge: the check below is what
            # fails, and the base build fails it the same way for having no signal at all.
            shown = wait_until(20, lambda: badge_shows(1, True))
            step("the launcher entry shows 1", shown)
            return 1

        window = popen([tiny_bin], cwd=data, env=env, stdin=subprocess.DEVNULL)
        opened = wait_until(20, lambda: window.poll() is None)
        step("the window is open", opened)
        if not opened:
            return 1

        shown = wait_until(45, lambda: badge_shows(1, True))
        step("the launcher entry shows 1 while Bea's direct is unread", shown)
        if not shown:
            return 1

        state.mark_read()
        cleared = wait_until(45, lambda: badge_shows(0, False))
        step("the launcher entry is cleared once the direct was read", cleared)
        if not cleared:
            return 1
        time.sleep(2.0)
        step("the entry stays cleared", badge_shows(0, False))
    finally:
        stop(window)
        stop(chatd)
        stop(MONITOR)
        fixture.stop(httpd, state)
        with open(os.path.join(OUT, "fixture.log"), "w", encoding="utf-8") as handle:
            handle.write("\n".join(state.log) + "\n")
    return 1 if FAILED else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        step("driver: %s" % error, False)
        print("driver: %s" % error, file=sys.stderr)
        sys.exit(1)
