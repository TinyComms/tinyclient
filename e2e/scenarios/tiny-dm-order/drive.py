#!/usr/bin/env python3
"""Drive chatd against fixture.py and read the direct back in the order chatd pages it.

Both posts share one stamp. The id that sorts first is the later post. The page order is the
order the window draws, so "first" then "second" is the fix and the other way is the bug.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR, E2E_SABOTAGE=1 (no session, so the posts never
arrive). E2E_NO_WINDOW=1 skips opening the window, for a run with no display.
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
    return tempfile.mkdtemp(prefix="tldm-")


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


def connect(path):
    last = "no database"
    for _ in range(10):
        try:
            return sqlite3.connect(path, timeout=2), ""
        except sqlite3.Error as error:
            last = str(error)
            time.sleep(0.1)
    return None, last


def stored(data):
    """The direct's posts in the order chatd pages them, or why that order cannot be read."""
    path = os.path.join(data, "chat.sqlite")
    if not os.path.exists(path):
        return None, "no database"
    con, why = connect(path)
    if con is None:
        return None, why
    try:
        have = [row[1] for row in con.execute("PRAGMA table_info(message)")]
        texts = [row[0] for row in con.execute(
            "SELECT text FROM message WHERE conversation = ?", (fixture.DIRECT,)
        )]
        if "seq" not in have:
            return None, "no sequence column (%s)" % ", ".join(texts)
        page = [row[0] for row in con.execute(
            "SELECT text FROM message WHERE conversation = ? "
            "ORDER BY sent, (seq = 0), seq, length(id), id",
            (fixture.DIRECT,),
        )]
        return page, ""
    except sqlite3.Error as error:
        return None, str(error)
    finally:
        con.close()


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
            page, why = stored(data)
            step("sabotage stores no direct", page is None)
            step("first then second", False)
            return 1

        def both():
            page, _why = stored(data)
            return page is not None and "first" in page and "second" in page

        arrived = wait_until(30, both)
        page, why = stored(data)
        step("both posts are stored (%s)" % (page if page is not None else why), arrived)
        if not arrived:
            return 1
        kept = page == ["first", "second"]
        detail = "first then second"
        if not kept:
            detail += " (page %s)" % page
        step(detail, kept)
        if not kept:
            return 1

        if not NO_WINDOW:
            window = popen([tiny_bin], cwd=data, env=env, stdin=subprocess.DEVNULL)
            shown = wait_until(20, lambda: window.poll() is None)
            step("window stays open", shown)
            if shown:
                time.sleep(2.0)
                page, why = stored(data)
                step("the order holds after the window opens", page == ["first", "second"])
                if why and page != ["first", "second"]:
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
