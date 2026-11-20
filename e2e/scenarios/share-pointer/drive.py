#!/usr/bin/env python3
"""Share the real screen and check the pointer goes with it, or stays off when the share asks."""
import pathlib
import subprocess
import os
import sys

root = pathlib.Path(__file__).resolve().parents[3]
test = "the_capture_thread_shares_with_the_pointer_the_share_asked_for"
command = [os.environ.get("CARGO", "cargo"), "test", "-p", "mediad", "--lib", test, "--", "--ignored", "--nocapture"]
result = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
print(result.stdout, end="")
# An older build has no such test: cargo runs none and the markers are absent, which fails.
proved = (
    result.returncode == 0
    and "share-pointer-proof:" in result.stdout
    and "share-pointer-proof: a share that asks for none turns it off" in result.stdout
    and "test result: ok. 1 passed" in result.stdout
)
sys.exit(0 if proved else 1)
