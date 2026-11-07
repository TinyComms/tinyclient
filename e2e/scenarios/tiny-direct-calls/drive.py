#!/usr/bin/env python3
"""Run the current native direct-call probe against a private disposable fixture."""
import os
import pathlib
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[3]
fixture = os.environ.get("TINY_DIRECT_CALL_FIXTURE")
if not fixture or not pathlib.Path(fixture).is_file():
    sys.exit("TINY_DIRECT_CALL_FIXTURE must name an existing private fixture JSON")
command = [os.environ.get("CARGO", "cargo"), "test", "-p", "chatd", "--no-default-features", "--lib", "real_direct_call_round_trip", "--", "--ignored", "--nocapture"]
result = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
print(result.stdout, end="")
proved = result.returncode == 0 and "direct-call-proof: no media before accept; bidirectional PCM; mute; leave; decline; cancel" in result.stdout and "test result: ok. 1 passed" in result.stdout
sys.exit(0 if proved else 1)
