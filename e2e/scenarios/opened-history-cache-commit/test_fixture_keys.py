"""Scratch restarts must retain the same wrapping keys from their first launch."""

import base64
import json
from pathlib import Path
import tempfile
import unittest

import fixture_controller as fixture
import ipc


class FixtureKeysTests(unittest.TestCase):
    def test_missing_or_bad_wrapping_keys_refuse_before_any_profile_is_created(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            state = base64.b64encode(bytes(range(32))).decode()
            cache = base64.b64encode(bytes(range(32, 64))).decode()
            contract = {
                "marker": "opened-history-cache-commit-fern-clover", "conversation_id": "conversation",
                "receiver": {"user_id": "ada", "profile": str(root / "ada"),
                             "session": {"user_id": "ada", "display_name": "Ada", "e2ee": {"state": state, "cache": cache}}},
                "sender": {"user_id": "bea", "profile": str(root / "bea"),
                           "session": {"user_id": "bea", "display_name": "Bea", "e2ee": {"state": state, "cache": cache}}},
            }
            path = root / "fixture.json"
            for keys in (None, {}, {"state": state}, {"state": "not-base64", "cache": cache},
                         {"state": base64.b64encode(b"short").decode(), "cache": cache}):
                contract["receiver"]["session"]["e2ee"] = keys
                path.write_text(json.dumps(contract), encoding="utf-8")
                with self.subTest(keys_present=keys is not None), self.assertRaises(fixture.FixtureError):
                    fixture.load_contract(str(path))
                self.assertFalse((root / "ada").exists())
            contract["receiver"]["session"]["e2ee"] = {"state": state, "cache": cache}
            path.write_text(json.dumps(contract), encoding="utf-8")
            before = path.read_bytes()
            one = fixture.load_contract(str(path))
            two = fixture.load_contract(str(path))
            self.assertEqual(one, two)
            self.assertEqual(path.read_bytes(), before)
            contract["sender"]["session"]["display_name"] = "Bea Test"
            path.write_text(json.dumps(contract), encoding="utf-8")
            with self.assertRaisesRegex(fixture.FixtureError, "single-word"):
                fixture.load_contract(str(path))

    def test_malformed_state_never_counts_as_a_delivered_encrypted_message(self):
        marker = "opened-history-cache-commit-fern-clover"
        for state in (None, {}, {"other": "sent"}, {"state": "pending"},
                      {"state": "failed", "reason": "refused"}):
            with self.subTest(state=state):
                self.assertFalse(ipc.text_contains_marker({"op": "messages", "messages": [{
                    "id": "one", "text": marker, "seal": "encrypted", "state": state,
                }]}, marker))
        self.assertTrue(ipc.text_contains_marker({"op": "messages", "messages": [{
            "id": "one", "text": marker, "seal": "encrypted", "state": {"state": "sent"},
        }]}, marker))


if __name__ == "__main__":
    unittest.main()
