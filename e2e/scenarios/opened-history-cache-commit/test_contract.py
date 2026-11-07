"""Regression checks for fixture identity and authenticated-history diagnostics."""

import json
import os
import subprocess
import sys
import unittest

import ipc
import session_contract


class ContractTests(unittest.TestCase):
    def test_scratch_child_receives_tiny_server_session_without_other_accounts(self):
        session = {"user_id": "receiver", "token": "synthetic-only"}
        env = session_contract.environment(
            {**os.environ, "TINYCLIENT_SESSION_DISCORD": "unrelated",
             "TINYCLIENT_SESSION_TINYSERVER": "wrong"},
            "scratch-receiver", session,
        )
        child = subprocess.run(
            [sys.executable, "-c",
             "import os,json; print(json.dumps({k:v for k,v in os.environ.items() "
             "if k.startswith('TINYCLIENT_SESSION_')}))"],
            env=env, text=True, capture_output=True, check=True,
        )
        self.assertEqual(json.loads(child.stdout), {
            "TINYCLIENT_SESSION_TINY SERVER": json.dumps(session, separators=(",", ":")),
        })

    def test_marker_must_be_opened_message_text(self):
        marker = "opened-history-cache-commit-only-target"
        for answer in (
            {"op": "error", "message": marker},
            {"op": "messages", "messages": [{"text": "other", "seal": "encrypted",
                                             "reply_to": {"text": marker}}]},
            {"op": "messages", "messages": [{"text": marker, "seal": "undecryptable"}]},
            {"op": "messages", "messages": [{"text": marker, "seal": "encrypted", "state": "sending"}]},
            {"op": "messages", "messages": [{"text": marker + "-suffix", "seal": "encrypted"}]},
        ):
            with self.subTest(answer=answer):
                self.assertFalse(ipc.text_contains_marker(answer, marker))
        self.assertTrue(ipc.text_contains_marker({
            "op": "messages", "messages": [
                {"id": "message-one", "text": marker, "seal": "encrypted"},
            ],
        }, marker))

    def test_missing_pixel_observation_cannot_certify_history_recovery(self):
        import drive
        self.assertEqual(drive.classify_case("opened", "undecryptable", False, False), "setup-incomplete")
        self.assertEqual(drive.classify_case("undecryptable", "opened", False, False), "setup-incomplete")
        self.assertEqual(drive.classify_case("opened", "undecryptable", True, False), "base-history-loss-red")
        self.assertEqual(drive.classify_case("undecryptable", "opened", False, True), "branch-recoverable-green")
        self.assertEqual(drive.classify_case("missing", "missing", False, False), "setup-incomplete")
        self.assertEqual(drive.classify_case("opened", "missing", True, False), "setup-incomplete")
        self.assertEqual(drive.classify_case("opened", "undecryptable", True, None), "setup-incomplete")
        self.assertEqual(drive.classify_case("undecryptable", "opened", None, True), "setup-incomplete")

    def test_target_observation_waits_for_exact_scoped_row_not_empty_backfill(self):
        key = {"network": "tiny", "account": "receiver", "id": "conversation"}
        marker = "opened-history-cache-commit-only-target"
        def answer(rows, scope=key):
            return {"op": "messages", "conversation": scope, "messages": rows}
        for payload in (
            {"op": "error", "message": "offline"},
            answer([], {**key, "account": "other"}),
        ):
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                ipc.target_state(payload, key, "target-id", marker)
        self.assertEqual(ipc.target_state(answer([]), key, "target-id", marker), "missing")
        self.assertEqual(ipc.target_state(answer([
            {"id": "other-id", "text": marker, "seal": "encrypted"},
        ]), key, "target-id", marker), "missing")
        self.assertEqual(ipc.target_state(answer([
            {"id": "target-id", "seal": "undecryptable"},
        ]), key, "target-id", marker), "undecryptable")
        self.assertEqual(ipc.target_state(answer([
            {"id": "target-id", "text": marker, "seal": "encrypted"},
        ]), key, "target-id", marker), "opened")


    def test_peer_delivery_requires_encrypted_sent_server_id(self):
        key = {"network": "tiny", "account": "sender", "id": "conversation"}
        marker = "opened-history-cache-commit-only-target"
        message_id = "11111111-1111-4111-8111-111111111111"
        payload = {"op": "messages", "conversation": key, "messages": [
            {"id": message_id, "text": marker, "seal": "encrypted", "state": "sending"},
        ]}
        self.assertIsNone(ipc.delivered_id(payload, key, marker))
        payload["messages"][0].pop("state")
        self.assertEqual(ipc.delivered_id(payload, key, marker), message_id)
        payload["messages"][0]["id"] = "temporary-local-id"
        self.assertIsNone(ipc.delivered_id(payload, key, marker))

    def test_backfill_is_observed_until_exact_row_arrives(self):
        from unittest.mock import patch
        import drive
        key = {"network": "tiny", "account": "receiver", "id": "conversation"}
        marker = "opened-history-cache-commit-only-target"
        initial = {"op": "messages", "conversation": key, "messages": []}
        arrived = {**initial, "messages": [{"id": "target-id", "seal": "undecryptable"}]}
        contract = {"receiver": {"user_id": "receiver"},
                    "conversation_id": "conversation", "marker": marker}
        with patch.object(ipc, "messages", side_effect=[initial, arrived]):
            state, page = drive.wait_target("scratch", "bundle", contract, "target-id", seconds=2)
        self.assertEqual(state, "undecryptable")
        self.assertEqual(page["messages"][0]["id"], "target-id")


if __name__ == "__main__":
    unittest.main()
