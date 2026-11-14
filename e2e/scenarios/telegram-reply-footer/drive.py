#!/usr/bin/env python3
"""Open a real Telegram channel pane and read the footer from its window pixels."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

OUT = Path(os.environ["E2E_OUT"]).resolve()
APP = Path(os.environ["E2E_APP_DIR"]).resolve() / "Contents" / "MacOS"
HELPER = os.environ["E2E_HELPER"]
T0 = float(os.environ.get("E2E_T0") or time.time())


def step(words, ok):
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    with (OUT / "steps.jsonl").open("a", encoding="utf-8") as log:
        log.write(json.dumps({"t": round(time.time() - T0, 2), "step": words, "ok": bool(ok)}) + "\n")
    if not ok:
        raise RuntimeError(words)


def until(predicate, seconds=25):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(0.25)
    return None


def window_text(pid):
    path = OUT / "window-text.json"
    path.unlink(missing_ok=True)
    helper = Path(HELPER)
    launch = ["open", "-n", "-a", HELPER, "--args"] if helper.suffix == ".app" else [HELPER]
    subprocess.run([*launch, "text", "--pid", str(pid), "--out", str(path)],
                   check=True, timeout=20)
    if not until(path.exists, 10):
        raise RuntimeError("The lab helper did not read the window")
    seen = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(seen, str):
        raise RuntimeError(seen)
    return " ".join(" ".join(seen).lower().split())


def stop(proc):
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def run(data):
    env = {name: value for name, value in os.environ.items() if not name.startswith("TINYCLIENT_SESSION_")}
    env.update(TINYCLIENT_DIR=str(data), TINYCLIENT_TELEGRAM_FAKE="30", TINYCLIENT_TELEGRAM_T0=str(int(time.time())),
               TINYCLIENT_SESSION_TELEGRAM='{"user":7001,"name":"Mira Kessler","home":2,"dcs":[]}',
               TINYCLIENT_LOG_LEVEL="info")
    window = None
    with (OUT / "app.log").open("wb") as log:
        daemon = subprocess.Popen([APP / "chatd"], env=env, stdout=log, stderr=log)
        try:
            def connected():
                result = subprocess.run([APP / "tinyclient", "--text"], env=env, capture_output=True, text=True, timeout=5)
                return "Telegram Connected" in result.stdout

            step("The invented Telegram account is connected", until(connected))
            channel = json.dumps({"network": "telegram", "account": "7001", "id": "c9001"})
            window = subprocess.Popen([APP / "tinyclient", "--open=" + channel], env=env, stdout=log, stderr=log)
            (OUT / "app.pid").write_text(str(window.pid), encoding="utf-8")
            calls = data / "telegram.calls"
            step("The Field Notes channel is open", until(lambda: calls.exists() and
                 "GetSponsoredMessages c9001" in calls.read_text(encoding="utf-8")))
            with (data / "telegram.ctl").open("a", encoding="utf-8") as control:
                control.write("comments\n")

            def footer():
                text = window_text(window.pid)
                return text if re.search(r"\b2 replies\b", text) else None

            seen = until(footer)
            step("The post shows a 2 replies link before its comments have been opened", seen)
            step("The footer omits the unknown last reply time", "last reply" not in seen)
            time.sleep(2)
        finally:
            stop(window)
            stop(daemon)


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    try:
        with tempfile.TemporaryDirectory(prefix="tiny-tg-footer-", dir="/tmp") as directory:
            run(Path(directory))
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        print(error, flush=True)
        raise SystemExit(1)
