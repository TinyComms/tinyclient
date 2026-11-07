"""Small public-safe bridge from the private scenario ledger to the lab."""
import json
from pathlib import Path
import re


def write_lab_result(out, summary, probe):
    root = Path(out)
    platforms = {}
    for platform in ("windows", "linux"):
        recording, steps = root / platform / "recording-raw.mp4", root / platform / "steps.jsonl"
        digest = (summary.get("binary_sha256", {}).get("tinyclient.exe") if platform == "windows"
                  else summary.get("platform_binary_sha256", {}).get(platform))
        if not recording.is_file() or not steps.is_file() or not re.fullmatch("[0-9a-f]{64}", digest or ""):
            continue
        info = probe(str(recording))
        if not info or info.get("seconds", 0) <= 0:
            continue
        platforms[platform] = {"recording": platform + "/recording-raw.mp4", "steps": platform + "/steps.jsonl",
                               "seconds": info["seconds"], "binary_sha256": digest,
                               "vfr": platform == "windows"}
    result = {"schema": 1, "execution": "coordinated-ipc", "client_sha": summary.get("client_sha"),
              "server_sha": summary.get("server", {}).get("tinyserver_sha"),
              "completed": summary.get("verdict") in ("pass", "fail", "base-as-expected") and len(platforms) == 2,
              "platforms": platforms}
    (root / "lab-result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result
