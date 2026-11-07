import json
from pathlib import Path
import subprocess
import unittest
from unittest import mock

import dl
from lib.devices import LinuxSsh


class LabParticipants(unittest.TestCase):
    def test_linux_profile_and_binary_are_pinned_in_every_host_action(self):
        try:
            device = LinuxSsh("a_linux", {"user_id": "ada"}, "tiny-linux", "~/tiny-dl", "Ada",
                              app_dir="~/tiny-dl/bin/pinned", state_root="/tmp/dl-fixture1")
        except TypeError:
            self.fail("Linux participant has no isolated profile / immutable binary inputs")
        self.assertEqual(device.dir, "/tmp/dl-fixture1/a_linux")
        with mock.patch("subprocess.run", return_value=subprocess.CompletedProcess([], 0, "", "")) as run:
            device.sh("bash ~/tiny-dl/linux-host.sh stop a_linux")
        command = run.call_args.args[0][-1]
        self.assertIn("DL_STATE_ROOT=/tmp/dl-fixture1", command)
        self.assertIn('DL_APP_DIR="$HOME"/tiny-dl/bin/pinned', command)

    def test_cli_cleanup_only_accepts_explicit_owned_state(self):
        with mock.patch("sys.argv", ["dl.py", "cleanup", "--state", "owned-state"]), \
             mock.patch.object(dl, "Scenario", side_effect=AssertionError("fixture action in cleanup")), \
             mock.patch("lib.lifecycle.cleanup", return_value={"windows": 0, "linux": 0}) as cleanup:
            try:
                result = dl.main()
            except SystemExit:
                self.fail("scenario CLI has no scoped cleanup action")
        self.assertEqual(result, 0)
        self.assertEqual(cleanup.call_args.args, ("owned-state",))


if __name__ == "__main__":
    unittest.main()
