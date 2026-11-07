import json
from pathlib import Path
import tempfile
import unittest

try:
    from lib.lab_result import write_lab_result
except ImportError:
    write_lab_result = None


class LabResult(unittest.TestCase):
    def test_manifest_retains_native_sources_timings_and_raw_recordings(self):
        self.assertIsNotNone(write_lab_result, "scenario has no lab manifest producer")
        with tempfile.TemporaryDirectory() as out:
            for p in ("windows", "linux"):
                folder = Path(out) / p
                folder.mkdir()
                (folder / "recording-raw.mp4").write_bytes(b"fixture")
                (folder / "steps.jsonl").write_text('{"t":1,"step":"ready","ok":true}\n')
            summary = {"client_sha": "a" * 40, "server": {"tinyserver_sha": "b" * 40},
                       "verdict": "pass", "binary_sha256": {"tinyclient.exe": "c" * 64},
                       "platform_binary_sha256": {"linux": "d" * 64}}
            write_lab_result(out, summary, lambda path: {"seconds": 7.5})
            result = json.loads((Path(out) / "lab-result.json").read_text())
            self.assertTrue(result["completed"])
            self.assertEqual(result["client_sha"], "a" * 40)
            self.assertEqual(result["platforms"]["linux"]["binary_sha256"], "d" * 64)
            self.assertEqual(result["platforms"]["windows"]["recording"], "windows/recording-raw.mp4")
            self.assertEqual(result["platforms"]["windows"]["seconds"], 7.5)

    def test_missing_native_recording_leaves_incomplete_manifest(self):
        self.assertIsNotNone(write_lab_result, "scenario has no lab manifest producer")
        with tempfile.TemporaryDirectory() as out:
            write_lab_result(out, {"verdict": "pass"}, lambda path: None)
            result = json.loads((Path(out) / "lab-result.json").read_text())
            self.assertFalse(result["completed"])
            self.assertEqual(result["platforms"], {})
