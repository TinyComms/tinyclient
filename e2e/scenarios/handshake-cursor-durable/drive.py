#!/usr/bin/env python3
"""Source component proof only: real MLS handshake faults and cold/warm retry."""
import json
import os
from pathlib import Path
import re
import subprocess
import time

PREFIX = "adapters::tiny::e2ee::receive_tests::"
TESTS = (
    ("failed_welcome_save_keeps_the_feed_retryable_after_restart_or_in_memory",
     "failed Welcome save remains retryable after cold restart and in-memory retry"),
    ("failed_commit_save_keeps_new_ciphertext_readable_after_restart_or_in_memory",
     "failed membership commit save preserves new ciphertext through cold and warm retry"),
)


def main():
    out_value = os.environ.get("E2E_OUT")
    if not out_value:
        raise SystemExit("E2E_OUT is required")
    out = Path(out_value).resolve()
    out.mkdir(parents=True, exist_ok=True)
    source = Path(os.environ.get("E2E_SRC_DIR") or Path(__file__).resolve().parents[3])
    started = float(os.environ.get("E2E_T0") or time.time())
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = str(out / "scratch")
    steps = out / "steps.jsonl"
    steps.write_text("", encoding="utf-8")
    all_passed = True
    for index, (name, label) in enumerate(TESTS):
        test = PREFIX + name
        passed = False
        log = out / f"component-{index + 1}.log"
        try:
            result = subprocess.run(
                [env.get("CARGO", "cargo"), "test", "--locked", "-p", "chatd", "--lib", test,
                 "--", "--exact", "--nocapture"],
                cwd=source, env=env, capture_output=True, text=True, timeout=600,
            )
            output = result.stdout + result.stderr
            log.write_text(output, encoding="utf-8")
            passed = result.returncode == 0 and all(re.search(pattern, output, re.M) for pattern in (
                r"^running 1 test$", r"^test " + re.escape(test) + r" \.\.\. ok$",
                r"^test result: ok\. 1 passed; 0 failed; 0 ignored;",
            ))
        except (OSError, subprocess.TimeoutExpired) as error:
            log.write_text(str(error), encoding="utf-8")
        with steps.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps({
                "t": round(time.time() - started, 3), "step": label, "ok": bool(passed),
            }) + "\n")
        all_passed = all_passed and passed
    return 0 if all_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
