import argparse
import contextlib
import io
import os
import tempfile
import unittest
from unittest.mock import patch

import dl


class ProofBoundaryTest(unittest.TestCase):
    def test_refuses_reusing_an_evidence_directory(self):
        with tempfile.TemporaryDirectory() as out:
            with open(os.path.join(out, "evidence.txt"), "w") as file:
                file.write("keep existing proof")
            with self.assertRaisesRegex(ValueError, "not empty"):
                dl.Scenario(argparse.Namespace(out=out))

    def test_unknown_device_fails_before_starting_a_fixture(self):
        with tempfile.TemporaryDirectory() as out:
            with open(os.path.join(out, "summary.json"), "w") as file:
                file.write('{"verdict":"pass"}')
            with patch.object(dl, "Scenario") as scenario, patch("sys.argv", ["dl.py", "run", "--devices", "unknown"]):
                scenario.return_value.out = out
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    dl.main()
                self.assertEqual(error.exception.code, 2)
                scenario.assert_not_called()
