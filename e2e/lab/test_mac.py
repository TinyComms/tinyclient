"""The Mac adapters' deadline must hold when the Mac says nothing."""
import os
import sys
import time
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "platforms"))
import _mac  # noqa: E402

PY = sys.executable


class Deadline(unittest.TestCase):
    def test_a_silent_child_is_stopped_at_the_deadline(self):
        started = time.time()
        with self.assertRaises(_mac.MacError):
            _mac._stream([PY, "-c", "import time; time.sleep(120)"], b"", lambda line: None, timeout=2)
        self.assertLess(time.time() - started, 15)

    def test_a_child_that_prints_once_then_stalls_is_stopped_too(self):
        lines = []
        started = time.time()
        with self.assertRaises(_mac.MacError):
            _mac._stream([PY, "-u", "-c", "print('hello'); import time; time.sleep(120)"], b"", lines.append, timeout=3)
        self.assertLess(time.time() - started, 15)
        self.assertIn("mac: hello", lines)

    def test_a_chatty_child_that_finishes_returns_its_output_and_status(self):
        status, text = _mac._stream([PY, "-c", "print('one'); print('two'); raise SystemExit(3)"], b"", lambda line: None, timeout=30)
        self.assertEqual((status, text), (3, "one\ntwo"))

    def test_input_reaches_the_child(self):
        status, text = _mac._stream([PY, "-c", "import sys; print(sys.stdin.read().upper())"], b"abc", lambda line: None, timeout=30)
        self.assertEqual((status, text), (0, "ABC"))

    def test_the_login_shell_noise_is_dropped(self):
        code = "print(\"(eval):1: can't change option: zle\"); print('real')"
        _, text = _mac._stream([PY, "-c", code], b"", lambda line: None, timeout=30)
        self.assertEqual(text, "real")


if __name__ == "__main__":
    unittest.main()
