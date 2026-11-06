"""Offline recovery orchestration checks. No live app/profile is opened."""
import copy
import json
from pathlib import Path
import tempfile
import time
import unittest

from lib import recovery_runtime as runtime


CONV = "11111111-1111-4111-8111-111111111111"
A = "22222222-2222-4222-8222-222222222222"
B = "33333333-3333-4333-8333-333333333333"
GATE = "55555555-5555-4555-8555-555555555555"


class RuntimePreparationTest(unittest.TestCase):
    def test_namespace_is_exclusive_and_never_reuses_an_existing_profile(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            existing = parent / "proof-mobile-next"
            existing.mkdir()
            marker = existing / "preserve"
            marker.write_bytes(b"original")
            with self.assertRaises(ValueError):
                runtime.reserve_namespace(parent, "proof-mobile-next")
            fresh = runtime.reserve_namespace(parent, "recovery-owned-test")
            self.assertEqual(fresh.parent, parent)
            with self.assertRaises(FileExistsError):
                runtime.reserve_namespace(parent, "recovery-owned-test")
            self.assertEqual(marker.read_bytes(), b"original")

    def test_wrapping_keys_are_unique_per_device_and_original_seed_is_unchanged(self):
        seed = {"sessions": {name: {"user_id": "ada", "token": "private"}
                             for name in ("a", "bea", "x", "y")}}
        original = copy.deepcopy(seed)
        fresh = runtime.fixture_sessions(seed)
        self.assertEqual(seed, original)
        keys = [key for session in fresh["sessions"].values() for key in session["e2ee"].values()]
        self.assertEqual(len(set(keys)), 8)
        for key in keys:
            import base64
            self.assertEqual(len(base64.b64decode(key, validate=True)), 32)
        with self.assertRaises(ValueError):
            runtime.fixture_sessions(fresh)

    def test_gate_preserves_other_controls_and_release_changes_only_state(self):
        with tempfile.TemporaryDirectory() as directory:
            control = Path(directory) / "control.json"
            control.write_text(json.dumps({"delay_seconds": .1}))
            gate = runtime.arm_gate(control, CONV, [A, B], seconds=90, gate_id=GATE)
            before = json.loads(control.read_text())
            self.assertEqual(before["delay_seconds"], .1)
            runtime.release_gate(control, gate)
            after = json.loads(control.read_text())
            self.assertEqual(after, dict(before, handshake_gate=dict(gate, state="released")))
            with self.assertRaises(ValueError):
                runtime.release_gate(control, dict(gate, expires_at=gate["expires_at"] + 1))

    def test_gate_requires_both_owned_readers_and_rejects_any_fault(self):
        gate = {"id": GATE, "conversation_id": CONV, "device_ids": [A, B]}
        rows = [{"event": "handshake_gate_held", "gate_id": GATE,
                 "conversation_id": CONV, "device_id": device} for device in (A, B)]
        self.assertEqual(runtime.gate_readers(rows, gate, "held"), {A, B})
        with self.assertRaises(ValueError):
            runtime.gate_readers(rows[:1], gate, "held")
        for fault in ("timeout", "capacity"):
            with self.assertRaises(ValueError):
                runtime.gate_readers(rows + [dict(rows[0], event="handshake_gate_" + fault)], gate, "held")

    def test_cursor_check_does_not_treat_directory_members_as_authenticated_mls_roster(self):
        before = {"a": {"cursor": 4}, "bea": {"cursor": 4}}
        after = {"a": {"cursor": 4}, "bea": {"cursor": 4}}
        self.assertTrue(runtime.assert_stalled(before, after, 5))
        after["bea"]["cursor"] = 5
        with self.assertRaises(ValueError):
            runtime.assert_stalled(before, after, 5)
        with self.assertRaises(ValueError):
            runtime.assert_stalled(before, before, 4)

    def test_journal_requires_order_and_never_claims_native_proof_from_ipc(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = runtime.Journal(Path(directory))
            with self.assertRaises(ValueError):
                journal.record("released", {})
            journal.record("prepared", {"artifact": "public hash"})
            with self.assertRaises(ValueError):
                journal.record("prepared", {})
            self.assertFalse(journal.summary()["release"])
            self.assertFalse(journal.summary()["native_proof"])
            self.assertIn("native-desktop-repair", journal.summary()["gaps"])
            self.assertNotIn("token", json.dumps(journal.summary()))

    def test_phase_is_checked_before_any_live_action(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = runtime.Journal(Path(directory))
            self.assertTrue(callable(getattr(journal, "require", None)), "pre-action phase guard missing")
            journal.require("prepared")
            with self.assertRaises(ValueError):
                journal.require("armed")

    def test_refusal_requires_a_real_terminal_error_and_no_accepted_server_row(self):
        self.assertTrue(callable(getattr(runtime, "assert_send_refused", None)), "send oracle missing")
        reason = "A device in this conversation isn't approved by its owner, so nothing was sent."
        runtime.assert_send_refused("client-1", 4, {"op": "error", "code": "refused", "detail": reason}, [], [])
        runtime.assert_send_refused("client-1", 4, {"op": "error", "detail": reason, "observation": "outbox_failed"}, [], [])
        for unrelated in ("The server could not be reached.", "HTTP 503", "Permission denied"):
            with self.assertRaises(ValueError):
                runtime.assert_send_refused("client-1", 4, {"op": "error", "code": "refused", "detail": unrelated}, [], [])
        with self.assertRaises(ValueError):
            runtime.assert_send_refused("client-1", 4, {"op": "error", "detail": reason}, [], [{"id": "server-1", "client_id": "client-1"}])
        with self.assertRaises(ValueError):
            runtime.assert_send_refused("client-1", 0, {"op": "ok"}, [], [])
        with self.assertRaises(ValueError):
            runtime.assert_send_refused("client-1", 1, {"op": "error"}, [], [{"id": "server-1", "client_id": "client-1"}])
        with self.assertRaises(ValueError):
            runtime.assert_send_refused("client-1", 5, None, [], [])

    def test_nested_add_correlates_existing_member_commit_with_joiners_next_epoch_welcome(self):
        group = "11111111-1111-4111-8111-111111111111"
        old = [{"kind": "commit", "seq": 12, "epoch": 2, "generation": 0, "group_id": group}]
        new = [{"kind": "welcome", "seq": 13, "epoch": 3, "generation": 0, "group_id": group}]
        self.assertEqual(runtime.nested_add_coordinates(old, new, group, 2),
                         {"commit_seq": 12, "welcome_seq": 13, "epoch": 3})
        for bad in (dict(new[0], epoch=2), dict(new[0], seq=11), dict(new[0], group_id="22222222-2222-4222-8222-222222222222")):
            with self.assertRaises(ValueError):
                runtime.nested_add_coordinates(old, [bad], group, 2)

    def test_fence_oracle_rejects_generic_conflicts_and_http_permission_denials(self):
        self.assertTrue(callable(getattr(runtime, "assert_generation_fence", None)), "fence oracle missing")
        runtime.assert_generation_fence(409, {"error": "generation_changed"})
        for status, body in [(409, {"error": "stale_epoch"}), (403, {"error": "generation_changed"}), (409, None)]:
            with self.assertRaises(ValueError):
                runtime.assert_generation_fence(status, body)

    def test_join_control_must_be_real_ciphertext_from_the_named_device(self):
        self.assertTrue(callable(getattr(runtime, "assert_authored_ciphertext", None)), "origin oracle missing")
        row = {"id": "accepted", "e2ee": True, "ciphertext": "public-ciphertext",
               "device_id": A, "generation": 0, "group_id": CONV}
        runtime.assert_authored_ciphertext(row, CONV, A, 0)
        for field, value in (("device_id", B), ("generation", 1), ("ciphertext", ""), ("group_id", B), ("e2ee", False)):
            with self.subTest(field=field), self.assertRaises(ValueError):
                runtime.assert_authored_ciphertext(dict(row, **{field: value}), CONV, A, 0)

    def test_fences_require_captured_real_route_bodies_and_cover_tree_in_commit(self):
        root = f"/v1/e2ee/conversations/{CONV}"
        captured = {
            "create": {"method": "POST", "path": root + "/group", "body": {"device_id": A}},
            "commit": {"method": "POST", "path": root + "/commit", "body": {"device_id": A, "epoch": 0, "commit": "public"}},
            "tree": {"method": "POST", "path": root + "/commit", "body": {"device_id": A, "epoch": 0, "commit": "public", "ratchet_tree": "public"}},
            "message": {"method": "POST", "path": f"/v1/conversations/{CONV}/messages", "body": {"device_id": A, "epoch": 0, "ciphertext": "public"}},
            "edit": {"method": "POST", "path": f"/v1/conversations/{CONV}/messages", "body": {"device_id": A, "epoch": 0, "ciphertext": "public", "kind": "edit", "target_id": B}},
            "reaction": {"method": "POST", "path": root + "/reactions", "body": {"device_id": A, "epoch": 0, "ciphertext": "public"}},
            "proposal": {"method": "GET", "path": root, "body": None},
        }
        cases = runtime.fence_cases(CONV, 0, captured)
        self.assertEqual(len(cases), 14)
        self.assertTrue(all(case["expected_status"] == 409 for case in cases))
        self.assertEqual(cases[-1]["path"], root + "?generation=0")
        broken = copy.deepcopy(captured)
        broken["tree"]["body"].pop("ratchet_tree")
        with self.assertRaises(ValueError):
            runtime.fence_cases(CONV, 0, broken)
        broken = copy.deepcopy(captured)
        broken["message"]["path"] = "/v1/conversations/other/messages"
        with self.assertRaises(ValueError):
            runtime.fence_cases(CONV, 0, broken)


if __name__ == "__main__":
    unittest.main()
