#!/usr/bin/env python3
"""Wave 16B2 Telegram group call controls (#463, #468, #467, #454) desktop E2E driver.

Verifies chatd in E2E_APP_DIR reports native Telegram group audio, camera, and screen share
capabilities and writes structured step output to E2E_OUT/steps.jsonl. Full protocol, mediad,
GUI, and scripted-wire fake-server coverage lives in `protocol`, `mediad`, `tinyclient`, and
`chatd --test telegram`.

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


def chatd_bin(app_dir):
    if sys.platform == "win32":
        return os.path.join(app_dir, "chatd.exe")
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "chatd")
    return os.path.join(app_dir, "chatd")


def row_cell(stdout, label, col_idx):
    row = next(
        (line for line in stdout.splitlines() if line.startswith(f"| {label} |")),
        "",
    )
    cells = [c.strip() for c in row.strip("|").split("|")] if row else []
    return cells[col_idx] if len(cells) > col_idx else ""


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
    step("chatd binary present", True)

    proc = subprocess.run(
        [binary, "--capabilities"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
    )
    with open(os.path.join(OUT, "capabilities.md"), "w", encoding="utf-8") as cap_out:
        cap_out.write(proc.stdout)
    # Columns: Feature | Discord | Slack | Telegram | ...
    group_ok = row_cell(proc.stdout, "Group audio", 3) == "N"
    camera_ok = row_cell(proc.stdout, "Camera", 3) == "N"
    share_ok = row_cell(proc.stdout, "Screen share", 3) == "N"
    step("chatd reports Telegram group audio as native", proc.returncode == 0 and group_ok)
    step("chatd reports Telegram camera as native", proc.returncode == 0 and camera_ok)
    step("chatd reports Telegram screen share as native", proc.returncode == 0 and share_ok)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
