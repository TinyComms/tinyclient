"""Exercise attach orchestration with private temporary files and a device boundary."""
from pathlib import Path
import tempfile
import unittest

from lib import recovery_runtime as rt


class RecoveryDriverTest(unittest.TestCase):
    def test_attach_refuses_out_of_order_arm_before_any_control_file_write(self):
        import recovery_run
        self.assertTrue(callable(getattr(recovery_run, "RecoveryRun", None)), "attach orchestrator missing")
        with tempfile.TemporaryDirectory() as temp:
            state = rt.reserve_namespace(temp, "recovery-driver-test")
            out = Path(temp) / "evidence"
            out.mkdir()
            seed = rt.fixture_sessions({"sessions": {name: {"user_id": name, "token": "not-public"}
                                                     for name in ("a", "bea", "x", "y")}})
            rt.atomic_json(state / "private-sessions.json", seed)
            control = state / "control.json"
            control.write_text('{"other":true}')
            artifacts = {}
            for name in ("chatd", "tinyclient", "dlctl", "tinyserver"):
                path = state / (name + ".exe")
                path.write_bytes(name.encode())
                artifacts[name] = {"path": str(path), "sha256": rt.artifact_hash(path)}
            manifest = dict(state=str(state), seed=str(state / "private-sessions.json"),
                            proxy_control=str(control), wire=str(state / "wire.jsonl"),
                            base_url="http://localhost:19991", observer_url="http://127.0.0.1:19992",
                            devices={name: name for name in ("a", "bea", "x", "y")}, artifacts=artifacts,
                            key_persistence="unique-fixture-wrapping-keys",
                            sources={"client": "a" * 40, "server": "b" * 40},
                            conversation_id="11111111-1111-4111-8111-111111111111")
            # Factory construction is inert. Any IPC or request before the phase
            # guard would fail the test, rather than accidentally hitting an app.
            run = recovery_run.RecoveryRun(manifest, out, device_factory=lambda **kw: object())
            with self.assertRaises(ValueError):
                run.arm()
            self.assertEqual(control.read_text(), '{"other":true}')
            self.assertFalse((out / "recovery-journal.json").exists())
            with self.assertRaises(ValueError):
                rt.validate_manifest(dict(manifest, sources={}))

    def test_no_native_action_is_exposed_as_an_ipc_driver_command(self):
        import recovery_run
        self.assertTrue(callable(getattr(recovery_run, "parser", None)), "CLI missing")
        actions = recovery_run.parser()._actions[1].choices
        self.assertIn("repair-observed", actions)
        self.assertNotIn("repair-start", actions)
        self.assertNotIn("restart", actions)
        self.assertNotIn("cleanup", actions)


if __name__ == "__main__":
    unittest.main()
