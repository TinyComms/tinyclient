#!/usr/bin/env python3
"""An encrypted call's log says when its keys came and what the wait for them cost.

Runs one test of this build: two real mediad processes join an end-to-end encrypted room through
the test's own SFU, talk before the group has keys, and are then given them. The test reads each
mediad's log for the `keys_ready` stage and the count of frames held back, and prints a marker.
A build from before the change has no such test, so the filter runs nothing and this fails.

Env from the lab: E2E_OUT, E2E_T0 (both optional), CARGO.
"""

import os
import pathlib
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]
OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
TEST = "an_encrypted_join_logs_when_its_keys_came_and_what_the_wait_cost"
MARKER = "voice-keys-proof: the join's log has the keys_ready stage and the frames the wait for keys cost"


def step(words, ok):
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if OUT:
        os.makedirs(OUT, exist_ok=True)
        line = '{"t": %s, "step": "%s", "ok": %s}' % (round(time.time() - T0, 1), words, "true" if ok else "false")
        with open(os.path.join(OUT, "steps.jsonl"), "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def main():
    command = [os.environ.get("CARGO", "cargo"), "test", "-p", "mediad", "--test", "tiny", TEST, "--", "--exact", "--nocapture"]
    result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
    print(result.stdout, end="")
    if OUT:
        os.makedirs(OUT, exist_ok=True)
        with open(os.path.join(OUT, "app.out.log"), "w", encoding="utf-8") as handle:
            handle.write(result.stdout)
    # An empty filter also exits 0: the one test has to have run, and said so.
    proved = result.returncode == 0 and MARKER in result.stdout and "test result: ok. 1 passed" in result.stdout
    step("both mediads log the keys_ready stage and the frames the wait for keys cost", proved)
    return 0 if proved else 1


if __name__ == "__main__":
    sys.exit(main())
