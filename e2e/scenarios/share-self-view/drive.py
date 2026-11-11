#!/usr/bin/env python3
"""Share the real screen through mediad's capture thread and check my own share is painted for me."""
import pathlib
import subprocess
import os
import sys

root = pathlib.Path(__file__).resolve().parents[3]
test = "the_capture_thread_paints_my_own_share_from_a_real_screen"
command = [os.environ.get("CARGO", "cargo"), "test", "-p", "mediad", "--lib", test, "--", "--ignored", "--nocapture"]
result = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
print(result.stdout, end="")
# An older build has no such test: cargo runs none and the markers are absent, which fails.
proved = (
    result.returncode == 0
    and "share-self-view-proof:" in result.stdout
    and "share-self-view-proof: cleared when the share stopped" in result.stdout
    and "test result: ok. 1 passed" in result.stdout
)
sys.exit(0 if proved else 1)
