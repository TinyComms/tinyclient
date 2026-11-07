#!/usr/bin/env python3
"""Self-test for fixture_controller without chatd or network."""

from __future__ import annotations

import json
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path

import fixture_controller as fixture


class FixtureControllerTests(unittest.TestCase):
    def test_wipe_is_scoped_to_receiver_and_checks_conversation_before_delete(self):
        with tempfile.TemporaryDirectory() as tmp:
            profile = Path(tmp)
            conn = sqlite3.connect(profile / "chat.sqlite")
            conn.executescript("""
                CREATE TABLE conversation (network TEXT, account TEXT, id TEXT);
                CREATE TABLE message (network TEXT, account TEXT, conversation TEXT, id TEXT);
                INSERT INTO conversation VALUES ('tiny', 'receiver', 'target');
                INSERT INTO conversation VALUES ('tiny', 'other', 'target');
                INSERT INTO message VALUES ('tiny', 'receiver', 'target', 'mine');
                INSERT INTO message VALUES ('tiny', 'other', 'target', 'theirs');
                INSERT INTO message VALUES ('tiny', 'receiver', 'missing', 'orphan');
            """)
            conn.commit()
            conn.close()
            def rows(query):
                db = sqlite3.connect(profile / "chat.sqlite")
                try:
                    return db.execute(query).fetchall()
                finally:
                    db.close()
            contract = {"conversation_id": "target", "receiver": {
                "user_id": "receiver", "profile": str(profile),
            }}
            fixture.wipe_message_index(contract)
            self.assertEqual(rows("SELECT id FROM message ORDER BY id"),
                             [("orphan",), ("theirs",)])
            contract["conversation_id"] = "missing"
            with self.assertRaises(fixture.FixtureError):
                fixture.wipe_message_index(contract)
            self.assertEqual(rows("SELECT COUNT(*) FROM message WHERE id='orphan'"), [(1,)])

    def test_blocker_is_empty_directory_and_clears(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            profile = root / "receiver"
            user = "11111111-1111-4111-8111-111111111111"
            conv = "33333333-3333-4333-8333-333333333333"
            contract = {
                "marker": "opened-history-cache-commit-test",
                "conversation_id": conv,
                "receiver": {"user_id": user, "profile": str(profile)},
                "sender": {"user_id": "22222222-2222-4222-8222-222222222222", "profile": str(root / "sender")},
            }
            path = fixture.place_append_blocker(contract)
            self.assertTrue(path.is_dir())
            self.assertFalse(path.is_file())
            parent = path.parent
            self.assertTrue(parent.is_dir())
            (parent / "state.bin").write_bytes(b"keep-parent-writable")
            fixture.clear_append_blocker(contract)
            self.assertFalse(path.exists())
            self.assertEqual((parent / "state.bin").read_bytes(), b"keep-parent-writable")

    def test_wipe_keeps_conversation_row(self):
        with tempfile.TemporaryDirectory() as tmp:
            profile = Path(tmp) / "receiver"
            profile.mkdir()
            db = profile / "chat.sqlite"
            conv = "33333333-3333-4333-8333-333333333333"
            conn = sqlite3.connect(db)
            user = "11111111-1111-4111-8111-111111111111"
            conn.execute("CREATE TABLE conversation (network TEXT, account TEXT, id TEXT)")
            conn.execute("CREATE TABLE message (network TEXT, account TEXT, id TEXT, conversation TEXT, text TEXT)")
            conn.execute("INSERT INTO conversation VALUES ('tiny', ?, ?)", (user, conv))
            conn.execute(
                "INSERT INTO message VALUES ('tiny', ?, 'm1', ?, 'hello')",
                (user, conv),
            )
            conn.commit()
            conn.close()
            contract = {
                "marker": "opened-history-cache-commit-test",
                "conversation_id": conv,
                "receiver": {
                    "user_id": "11111111-1111-4111-8111-111111111111",
                    "profile": str(profile),
                },
                "sender": {
                    "user_id": "22222222-2222-4222-8222-222222222222",
                    "profile": str(Path(tmp) / "sender"),
                },
            }
            wiped = fixture.wipe_message_index(contract)
            self.assertEqual(wiped["after"], 0)
            self.assertEqual(wiped["conversation_rows"], 1)

    def test_missing_fixture_is_not_valid_red(self):
        env = os.environ.pop("E2E_CACHE_COMMIT_FIXTURE", None)
        try:
            with self.assertRaises(fixture.FixtureError) as raised:
                fixture.load_contract("")
            self.assertIn("not a valid base RED", str(raised.exception))
        finally:
            if env is not None:
                os.environ["E2E_CACHE_COMMIT_FIXTURE"] = env


if __name__ == "__main__":
    unittest.main()
