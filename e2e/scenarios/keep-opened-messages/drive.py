#!/usr/bin/env python3
"""A message already stored survives chatd opening a schema 8 cache.

The lab runs this once on the base build and once on this branch. The base drops
every table when the version changes, so the row is gone. This branch adds the
columns and the words stay.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR.
"""

import os
import sqlite3
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
        proc.wait(timeout=5)


def wait_until(limit, predicate):
    deadline = time.time() + limit
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def user_version(path):
    if not os.path.exists(path):
        return None
    try:
        con = sqlite3.connect(path, timeout=2)
        version = con.execute("PRAGMA user_version").fetchone()[0]
        con.close()
        return int(version)
    except sqlite3.Error:
        return None


def message_text(path):
    try:
        con = sqlite3.connect(path, timeout=2)
        row = con.execute("SELECT text FROM message WHERE id = ?", ("m1",)).fetchone()
        con.close()
        return None if row is None else row[0]
    except sqlite3.Error:
        return None


def park_schema_8(path):
    """Store one opened line, then make the file look like v0.2.56 wrote it."""
    con = sqlite3.connect(path, timeout=5)
    con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    con.execute(
        "INSERT INTO message (network, account, conversation, id, sent, text, pending, body) "
        "VALUES ('telegram', 'me', 'dm', 'm1', 1800000000, 'still here', 0, '{}')"
    )
    con.execute("DROP INDEX IF EXISTS conversation_preview_expires")
    con.execute("ALTER TABLE conversation DROP COLUMN preview_expires")
    con.execute("DROP INDEX IF EXISTS message_expires")
    con.execute("ALTER TABLE message DROP COLUMN expires")
    con.execute("ALTER TABLE message DROP COLUMN seq")
    con.execute("PRAGMA user_version = 8")
    con.commit()
    con.close()
    for suffix in ("-wal", "-shm"):
        side = path + suffix
        if os.path.exists(side):
            os.remove(side)


def start_chatd(binary, data):
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    return popen(
        [binary],
        cwd=data,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=open(os.path.join(OUT, "chatd.out.log"), "a", encoding="utf-8"),
        stderr=open(os.path.join(OUT, "chatd.err.log"), "a", encoding="utf-8"),
    )


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
    path = os.path.join(data, "chat.sqlite")
    chatd = None
    try:
        chatd = start_chatd(binary, data)
        created = wait_until(30, lambda: user_version(path) == 10 and chatd.poll() is None)
        step("chatd created a current store", created)
        if not created:
            return 1
        stop(chatd)
        chatd = None

        try:
            park_schema_8(path)
        except sqlite3.Error as error:
            step("the scratch store can be rewound to schema 8 (%s)" % error, False)
            return 1
        step("a schema 8 store holds the opened line", message_text(path) == "still here" and user_version(path) == 8)

        chatd = start_chatd(binary, data)
        opened = wait_until(30, lambda: user_version(path) == 10)
        step("chatd opened the schema 8 store", opened and chatd.poll() is None)
        kept = message_text(path) == "still here"
        step("the opened message is still in the store", kept)
    finally:
        stop(chatd)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
