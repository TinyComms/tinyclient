#!/usr/bin/env python3
"""UI Audit Wave 1 (core primitives, inputs, focus rings, light-mode contrast,
composer, conversation, sidebar/people/profile/pickers/chrome) desktop proof driver.

Starts a scratch demo chatd and runs tinyclient against it in E2E_APP_DIR.
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
    line = '{"t": %s, "step": "%s", "ok": %s}' % (
        round(now(), 1),
        words.replace('"', "'"),
        "true" if ok else "false",
    )
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def app_bin(app_dir, base):
    name = f"{base}.exe" if sys.platform == "win32" else base
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", name)
    return os.path.join(app_dir, name)


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()

    client_bin = app_bin(APP, "tinyclient")
    chatd_bin = app_bin(APP, "chatd")
    if not os.path.isfile(client_bin):
        step("tinyclient is in E2E_APP_DIR", False)
        return 1
    if not os.path.isfile(chatd_bin):
        step("chatd is in E2E_APP_DIR", False)
        return 1

    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000

    chatd = None
    try:
        with (
            open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as cout,
            open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8") as cerr,
        ):
            chatd = subprocess.Popen([chatd_bin, "--seed-demo"], stdout=cout, stderr=cerr, **kwargs)
            ready = False
            for _ in range(60):
                if chatd.poll() is not None:
                    break
                res = subprocess.run(
                    [client_bin, "--text"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    **kwargs,
                )
                if res.returncode == 0 and res.stdout.strip():
                    ready = True
                    break
                time.sleep(0.5)
            step("seeded demo chatd responds to tinyclient --text", ready)
    finally:
        if chatd is not None and chatd.poll() is None:
            chatd.terminate()
            try:
                chatd.wait(timeout=5)
            except subprocess.TimeoutExpired:
                chatd.kill()
                chatd.wait(timeout=5)

    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
