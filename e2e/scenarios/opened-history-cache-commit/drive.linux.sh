#!/usr/bin/env bash
# opened-history-cache-commit: person-facing proof is Windows-native only.
# A missing-platform run must not be labeled as the sealed-cache history-loss RED.
set -euo pipefail
OUT="${E2E_OUT:?}"
T0="${E2E_T0:-0}"
mkdir -p "$OUT"
python3 - <<'PY' "$OUT" "$T0"
import json, sys, time
out, t0 = sys.argv[1], float(sys.argv[2])
steps = out + "/steps.jsonl"
open(steps, "w").close()
line = {
    "t": round(time.time() - t0, 1),
    "step": "Windows native pixel driver is required; Linux is not claimed for this matched case",
    "ok": False,
}
open(steps, "a", encoding="utf-8").write(json.dumps(line) + "\n")
print("[FAILED]", line["step"], flush=True)
open(out + "/setup-incomplete.json", "w", encoding="utf-8").write(
    json.dumps({"error": "linux-not-claimed", "valid_red": False}, indent=2) + "\n"
)
sys.exit(1)
PY
