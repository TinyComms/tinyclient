#!/usr/bin/env python3
"""Drive chatd against fixture.py and read the badge back from the app.

The inbox lists directs, not channels, so `--text` is asked about Bea. Night Watch is the
channel shape the server really sends (no read cursor); its badge is read from the store the
window draws. Both must still be unread after the second, cursorless list says 0.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR, E2E_SABOTAGE=1 (no session, so the badge step
fails). E2E_NO_WINDOW=1 skips opening the window, for a run with no display.
"""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time

import fixture

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")
SABOTAGE = os.environ.get("E2E_SABOTAGE") == "1"
NO_WINDOW = os.environ.get("E2E_NO_WINDOW") == "1"
ME = fixture.ME
TOKEN = fixture.TOKEN

FAILED = False
STEPS = ""


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
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        mac = os.path.join(app_dir, "Contents", "MacOS")
        return os.path.join(mac, "chatd"), os.path.join(mac, "tinyclient")
    return os.path.join(app_dir, "chatd"), os.path.join(app_dir, "tinyclient")


def scratch_dir():
    if sys.platform == "win32":
        path = os.path.join(OUT, "data")
        os.makedirs(path, exist_ok=True)
        return path
    return tempfile.mkdtemp(prefix="tlur-")


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


def channel_unread(data):
    path = os.path.join(data, "chat.sqlite")
    if not os.path.exists(path):
        return None, "no database"
    last = "unread"
    # chatd keeps this file open under WAL. A second connection can read the committed row.
    for _ in range(10):
        try:
            con = sqlite3.connect(path, timeout=2)
            row = con.execute("SELECT unread FROM conversation WHERE id = ?", ("night-watch",)).fetchone()
            con.close()
            if row is None:
                return None, "no channel row"
            return int(row[0]), ""
        except sqlite3.Error as error:
            last = str(error)
            time.sleep(0.1)
    return None, last


def badges(tinyclient, env, data):
    try:
        code, text = inbox(tinyclient, env, data)
    except (OSError, subprocess.TimeoutExpired) as error:
        return None, None, "", str(error)
    if code != 0:
        return None, None, text, "tinyclient --text exited %s" % code
    unread, why = channel_unread(data)
    return badge_of(text, "Bea"), unread, text, why


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

        if SABOTAGE:
            direct, channel, _, _ = badges(tiny_bin, env, data)
            step("sabotage has no Tiny badge to keep", direct is None and channel is None)
            # A sabotage run is supposed to fail the real check. The line above records what
            # the empty directory showed; the scenario itself did not see the badge.
            step("Bea still shows 1 and Night Watch is still unread", False)
            return 1

        def published_twice():
            channels, directs, rooms = state.counts()
            return channels >= 2 and directs >= 2 and rooms >= 2

        listed = wait_until(30, published_twice)
        channels, directs, rooms = state.counts()
        step(
            "the live socket fetched the list again (channels %d, directs %d, rooms %d)" % (channels, directs, rooms),
            listed,
        )
        if not listed:
            return 1
        # The second response is already written. Give that publish time to store it.
        time.sleep(1.0)

        direct, channel, text, why = badges(tiny_bin, env, data)
        kept = direct == "1" and channel == 1
        detail = "Bea still shows 1 and Night Watch is still unread"
        if not kept:
            detail += " (Bea %s, Night Watch %s%s)" % (direct, channel, (", " + why) if why else "")
        step(detail, kept)
        if not kept:
            with open(os.path.join(OUT, "inbox.txt"), "w", encoding="utf-8") as handle:
                handle.write(text)
            return 1

        if not NO_WINDOW:
            window = popen([tiny_bin], cwd=data, env=env, stdin=subprocess.DEVNULL, creationflags=0)
            shown = wait_until(20, lambda: window.poll() is None)
            step("window stays open", shown)
            if shown:
                time.sleep(2.0)
                direct, channel, _, why = badges(tiny_bin, env, data)
                step(
                    "the badge is still there after the window's own refresh",
                    direct == "1" and channel == 1,
                )
                if why and not (direct == "1" and channel == 1):
                    print(why, flush=True)
    finally:
        stop(window)
        stop(chatd)
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
