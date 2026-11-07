import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

try:
    from lib import lifecycle
except ImportError:
    lifecycle = None


class Ownership(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(lifecycle, "no scoped device cleanup registry")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.state = self.tmp.name

    def test_empty_state_cleanup_is_noop(self):
        self.assertEqual(lifecycle.cleanup(self.state), {"windows": 0, "linux": 0})

    def test_linux_registration_is_committed_before_start_and_uses_exact_namespace(self):
        lifecycle.register_linux(self.state, "a_linux", "tiny-linux", "~/tiny-dl", "~/pinned/app", "/tmp/dl-fixture1")
        with mock.patch.object(lifecycle, "_remote_stop", return_value=None) as stop:
            result = lifecycle.cleanup(self.state)
        self.assertEqual(result, {"windows": 0, "linux": 1})
        entry = stop.call_args.args[0]
        self.assertEqual((entry["name"], entry["state_root"], entry["app_dir"]),
                         ("a_linux", "/tmp/dl-fixture1", "~/pinned/app"))

    def test_registry_for_another_state_cannot_stop_anything(self):
        lifecycle.register_linux(self.state, "a_linux", "tiny-linux", "~/tiny-dl", "~/pinned/app", "/tmp/dl-fixture1")
        path = Path(self.state) / "owned-devices.json"
        record = json.loads(path.read_text())
        record["state"] = str(Path(self.state) / "different")
        path.write_text(json.dumps(record))
        with mock.patch.object(lifecycle, "_remote_stop") as stop, self.assertRaisesRegex(ValueError, "ownership"):
            lifecycle.cleanup(self.state)
        stop.assert_not_called()

    def test_native_identity_mismatch_is_retained_and_not_killed(self):
        lifecycle.register_linux(self.state, "a_linux", "tiny-linux", "~/tiny-dl", "~/pinned/app", "/tmp/dl-fixture1")
        with mock.patch.object(lifecycle, "_remote_stop", side_effect=RuntimeError("identity mismatch")):
            with self.assertRaisesRegex(RuntimeError, "identity mismatch"):
                lifecycle.cleanup(self.state)
        self.assertTrue((Path(self.state) / "owned-devices.json").exists())

    @unittest.skipUnless(os.name == "nt", "native Windows process identity")
    def test_windows_cleanup_guards_creation_time_before_stopping_owned_child(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        self.addCleanup(lambda: child.kill() if child.poll() is None else None)
        record = lifecycle.register_windows(self.state, child.pid, sys.executable)
        changed = dict(record, creation_ticks=str(int(record["creation_ticks"]) + 1))
        with self.assertRaisesRegex(RuntimeError, "identity"):
            lifecycle._windows("stop", changed)
        self.assertIsNone(child.poll())
        self.assertEqual(lifecycle.cleanup(self.state), {"windows": 1, "linux": 0})
        child.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
