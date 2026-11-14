#!/usr/bin/env python3
"""Observe the real Tinyclient Inbox while an isolated Telegram session reconnects."""

import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time


OUT = Path(os.environ["E2E_OUT"]).resolve()
APP = Path(os.environ["E2E_APP_DIR"]).resolve()
T0 = float(os.environ.get("E2E_T0") or time.time())
WINDOWS = sys.platform == "win32"


def step(words, ok):
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    with (OUT / "steps.jsonl").open("a", encoding="utf-8") as out:
        out.write(json.dumps({"t": round(time.time() - T0, 2), "step": words, "ok": bool(ok)}) + "\n")
    if not ok:
        raise RuntimeError(words)


def until(predicate, seconds=25):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(0.2)
    return False


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def run(data, log):
    folder = APP / "Contents" / "MacOS" if APP.suffix == ".app" else APP
    suffix = ".exe" if WINDOWS else ""
    chatd, tinyclient = (folder / (name + suffix) for name in ("chatd", "tinyclient"))
    env = {k: v for k, v in os.environ.items() if not k.startswith("TINYCLIENT_SESSION_")}
    env.update(TINYCLIENT_DIR=str(data), TINYCLIENT_TELEGRAM_FAKE="30", TINYCLIENT_TELEGRAM_T0=str(int(time.time())),
               TINYCLIENT_SESSION_TELEGRAM='{"user":7001,"name":"Mira Kessler","home":2,"dcs":[]}',
               TINYCLIENT_LOG_LEVEL="info")

    def spawn(binary, console=False):
        return subprocess.Popen([str(binary)], cwd=data, env=env, stdout=log, stderr=log,
                                creationflags=0x08000000 if WINDOWS and console else 0)

    def inbox():
        result = subprocess.run([str(tinyclient), "--text"], cwd=data, env=env, capture_output=True, text=True,
                                timeout=8, creationflags=0x08000000 if WINDOWS else 0)
        return result.stdout if result.returncode == 0 else ""

    def steer(line):
        with (data / "telegram.ctl").open("a", encoding="utf-8") as control:
            control.write(line + "\n")

    def badge(text, title):
        prefix = "Telegram " + title.ljust(24) + " "
        return next((line[len(prefix):len(prefix) + 3].strip() for line in text.splitlines()
                     if line.startswith(prefix)), None)

    daemon = spawn(chatd, console=True)
    try:
        step("Telegram connects with a fresh profile", until(lambda: "Telegram Connected" in inbox()))
    finally:
        stop(daemon)

    # Fixture: the previous app session ended with three unread comments in the group.
    # While it was offline, the person left in another Telegram app. The fake's dialogs omit it.
    with sqlite3.connect(data / "chat.sqlite") as store:
        store.execute("INSERT INTO conversation(network, account, id, kind, title, unread, updated) "
                      "VALUES ('telegram', '7001', 'c9003', 'group', 'Field Notes chat', 3, ?)", (int(time.time()),))
        store.execute("INSERT OR REPLACE INTO name(network, account, id, value) "
                      "VALUES ('telegram', '7001', 'peer:c9003', ?)", ("9003009\tField Notes chat",))

    daemon = spawn(chatd, console=True)
    window = None
    try:
        step("Telegram reconnects after leaving the discussion group", until(lambda: "Telegram Connected" in inbox()))
        if not os.environ.get("E2E_NO_WINDOW"):
            window = spawn(tinyclient)
            (OUT / "app.pid").write_text(str(window.pid), encoding="utf-8")
            step("The Inbox window opens", until(lambda: any(
                b"tinyclient: window open" in path.read_bytes() for path in data.rglob("tinyclient.log")
            ), 15))
        before = inbox()
        (OUT / "inbox-after-reconnect.txt").write_text(before, encoding="utf-8")
        step("The left group and its old badge are gone from the Inbox", badge(before, "Field Notes chat") is None)

        steer("discussion-comment 80 84")
        steer("discussion-reply")
        step("One direct reply appears in Replies", until(lambda: badge(inbox(), "Replies") == "1"))
        after = inbox()
        (OUT / "inbox-after-reply.txt").write_text(after, encoding="utf-8")
        step("Activity on the later post did not bring the group back", badge(after, "Field Notes chat") is None)
        step("The direct reply is not in Saved Messages", "A reply to your comment" not in next(
            (line for line in after.splitlines() if line.startswith("Telegram Saved Messages")), ""))
        if window:
            time.sleep(2)  # Give the lab time to record the new Inbox row.
    finally:
        if window:
            stop(window)
        stop(daemon)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="tiny-tg-", dir=None if WINDOWS else "/tmp") as temp:
        with (OUT / "app.log").open("wb") as log:
            run(Path(temp), log)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, sqlite3.Error, subprocess.TimeoutExpired) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
