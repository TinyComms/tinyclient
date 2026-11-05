"""Coordinator contract tests: real ledgers/files, no fixture or desktop access."""
import dataclasses
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
import common
import lab

if importlib.util.find_spec("coordinated"):
    import coordinated
else:
    coordinated = None

SHA, SERVER = "a" * 40, "b" * 40
DIGEST = "c" * 64
SCENARIO = {"id": "device-sync", "execution": "coordinated-ipc", "release": False,
            "platforms": ["windows", "linux"], "required_args": ["--extended"],
            "needs": ["server-device-link", "server-self-sync", "two-people",
                      "device-sync-coordinator", "device-sync-extended"]}


class CoordinatorContract(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(coordinated, "lab has no coordinated scenario integration")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        mock.patch.dict(os.environ, {"LOCALAPPDATA": str(self.root / "out")}).start()
        self.app = self.root / "app"
        self.app.mkdir()
        (self.app / "SOURCE_SHA").write_text(SHA)
        self.scenario_dir = self.root / "scenario"
        (self.scenario_dir / "server").mkdir(parents=True)
        (self.scenario_dir / "dl.py").write_text("")
        exported = self.root / "source-input"
        (exported / "scripts/e2e").mkdir(parents=True)
        (exported / "scripts/e2e/win-window.ps1").write_text("pinned window helper")
        source_tar = self.root / "source.tar"
        with tarfile.open(source_tar, "w") as tar:
            tar.add(exported / "scripts", arcname="scripts")
        self.config = {"server_dir": str(self.root / "server"), "server_sha": SERVER,
                       "app_dirs": {SHA: str(self.app)}, "timeout_seconds": 60,
                       "linux_app_dirs": {SHA: "~/tiny-dl/bin/pinned"}, "advertise": "127.0.0.1",
                       "ports": {"pg": 55361, "http": 8193, "proxy": 8194, "oidc": 55362, "media": 50120}}
        self.ctx = common.Run("windows", "branch", "TinyComms/tinyclient", SHA, SHA,
                              SCENARIO, str(self.scenario_dir), str(source_tar),
                              str(self.root / "out/windows/branch/device-sync"), {}, lambda s: None)
        self.invocations, self.cleanups = [], []
        self.summary_changes, self.manifest_changes = {}, {}
        self.returncode = 0
        self.invoke = mock.patch.object(coordinated, "_invoke", side_effect=self.execute).start()
        self.addCleanup(mock.patch.stopall)
        self.cleanup = mock.patch.object(coordinated, "_cleanup", side_effect=lambda *a: self.cleanups.append(a)).start()

    def execute(self, command, env, log, timeout):
        self.invocations.append((command, env, timeout))
        out = Path(command[command.index("--out") + 1])
        out.mkdir(parents=True, exist_ok=True)
        platforms = {}
        for p in ("windows", "linux"):
            dest = out / p
            dest.mkdir()
            (dest / "recording-raw.mp4").write_bytes(b"raw fixture bytes")
            (dest / "steps.jsonl").write_text('{"t":1,"step":"linked","ok":true}\n')
            platforms[p] = {"recording": p + "/recording-raw.mp4", "steps": p + "/steps.jsonl",
                            "seconds": 8.0, "binary_sha256": DIGEST, "vfr": False}
        manifest = {"schema": 1, "execution": "coordinated-ipc", "client_sha": SHA,
                    "server_sha": SERVER, "completed": True, "platforms": platforms}
        manifest.update(self.manifest_changes)
        summary = {"client_sha": SHA, "scenario_sha": SHA, "server": {"tinyserver_sha": SERVER}, "verdict": "pass",
                   "counts": {"passed": 2, "failed": 0, "skipped": 1},
                   "assertions": [{"id": "B", "platform": "windows", "ok": True},
                                  {"id": "C", "platform": "linux", "ok": True},
                                  {"id": "X", "platform": "windows", "ok": None}]}
        summary.update(self.summary_changes)
        (out / "summary.json").write_text(json.dumps(summary))
        (out / "lab-result.json").write_text(json.dumps(manifest))
        return self.returncode

    def test_two_platforms_share_one_execution_and_keep_separate_evidence(self):
        session = coordinated.Session(self.config)
        windows = session.run(self.ctx)
        linux = session.run(dataclasses.replace(self.ctx, platform="linux", out_dir=str(self.root / "out/linux/branch/device-sync")))
        self.assertTrue(windows.passed and linux.passed)
        self.assertEqual(len(self.invocations), 1)
        self.assertEqual(len(self.cleanups), 1)
        self.assertIn("windows", windows.recording)
        self.assertIn("linux", linux.recording)
        self.assertEqual(windows.seconds, 8.0)

    def test_scenario_producer_and_lab_consumer_agree_on_native_manifest(self):
        path = Path(__file__).parent.parent / "scenarios/device-sync/lib/lab_result.py"
        spec = importlib.util.spec_from_file_location("device_sync_lab_result", path)
        producer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(producer)
        def execute(command, env, log, timeout):
            code = self.execute(command, env, log, timeout)
            out = Path(command[command.index("--out") + 1])
            summary = json.loads((out / "summary.json").read_text())
            summary["binary_sha256"] = {"tinyclient.exe": DIGEST}
            summary["platform_binary_sha256"] = {"linux": "d" * 64}
            (out / "summary.json").write_text(json.dumps(summary))
            producer.write_lab_result(out, summary, lambda path: {"seconds": 8.0})
            return code
        self.invoke.side_effect = execute
        session = coordinated.Session(self.config)
        windows = session.run(self.ctx)
        linux = session.run(dataclasses.replace(self.ctx, platform="linux"))
        self.assertTrue(windows.passed and linux.passed)
        self.assertEqual(linux.binary_sha256, "d" * 64)
        self.assertTrue(windows.vfr)
        self.assertFalse(linux.vfr)

    def test_run_uses_exact_sources_extended_cases_and_owned_fixture(self):
        coordinated.Session(self.config).run(self.ctx)
        command, env, timeout = self.invocations[0]
        self.assertEqual(command[command.index("--client-sha") + 1], SHA)
        self.assertEqual(command[command.index("--server-ref") + 1], SERVER)
        self.assertIn("--extended", command)
        self.assertEqual(command[command.index("--devices") + 1], "desk_b,linux")
        self.assertEqual(command[command.index("--cases") + 1], "reject,mitm,tamper,reuse,expired")
        self.assertNotIn("--no-reset", command)
        self.assertNotIn("--no-record", command)
        self.assertTrue(Path(env["DL_STATE"]).is_relative_to(self.root / "out"))
        self.assertEqual(timeout, 60)
        self.assertIn("--linux-state-dir", command)
        self.assertEqual(command[command.index("--linux-app-dir") + 1], "~/tiny-dl/bin/pinned")
        self.assertEqual(command[command.index("--media-port") + 1], "50120")
        self.assertTrue(Path(env["DL_STATE"]).is_relative_to(self.root / "out/tiny-e2e"))

    def test_export_includes_exact_application_helpers_and_head_scenario(self):
        (self.scenario_dir / "head-only.py").write_text("head scenario")
        coordinated.Session(self.config).run(self.ctx)
        command, env, _ = self.invocations[0]
        script = Path(command[1])
        client_root = script.parents[3]
        self.assertTrue((client_root / "scripts/e2e/win-window.ps1").is_file(), "exported scenario has no pinned app helpers")
        self.assertEqual((client_root / "scripts/e2e/win-window.ps1").read_text(), "pinned window helper")
        self.assertEqual((script.parent / "head-only.py").read_text(), "head scenario")
        self.assertEqual(env["E2E_SCENARIO_SHA"], SHA)
        self.assertTrue((client_root / "e2e/lab/common.py").is_file(), "export lacks the running lab's recording helper")

    def test_incomplete_base_is_an_infrastructure_error(self):
        self.summary_changes = {"verdict": "incomplete"}
        self.returncode = 1
        with self.assertRaisesRegex(ValueError, "incomplete"):
            coordinated.Session(self.config).run(dataclasses.replace(self.ctx, label="base"))
        self.assertEqual(len(self.cleanups), 1)

    def test_expected_base_regression_is_not_a_passing_run(self):
        self.summary_changes = {"verdict": "base-as-expected", "counts": {"passed": 1, "failed": 1, "skipped": 1},
                                "assertions": [{"id": "B", "platform": "windows", "ok": False, "expected_on_base": False},
                                               {"id": "C", "platform": "linux", "ok": True},
                                               {"id": "X", "platform": "windows", "ok": None}]}
        self.returncode = 1
        result = coordinated.Session(self.config).run(dataclasses.replace(self.ctx, label="base"))
        self.assertFalse(result.passed)
        self.assertTrue(result.recording)

    def test_changed_source_marker_is_rejected_before_fixture_actions(self):
        (self.app / "SOURCE_SHA").write_text("d" * 40)
        with self.assertRaisesRegex(ValueError, "source marker"):
            coordinated.Session(self.config).run(self.ctx)
        self.assertEqual(self.invocations, [])
        self.assertEqual(self.cleanups, [])

    def test_evidence_for_another_commit_is_rejected(self):
        self.manifest_changes = {"client_sha": "d" * 40}
        with self.assertRaisesRegex(ValueError, "provenance"):
            coordinated.Session(self.config).run(self.ctx)

    def test_ledger_from_another_scenario_commit_is_rejected(self):
        self.summary_changes = {"scenario_sha": "e" * 40}
        with self.assertRaisesRegex(ValueError, "provenance"):
            coordinated.Session(self.config).run(self.ctx)

    def test_a_pass_verdict_cannot_hide_failed_assertions(self):
        self.summary_changes = {"assertions": [{"id": "B", "platform": "windows", "ok": False},
                                               {"id": "C", "platform": "linux", "ok": True}]}
        with self.assertRaisesRegex(ValueError, "assertion ledger"):
            coordinated.Session(self.config).run(self.ctx)

    def test_base_label_cannot_turn_unexpected_failure_into_feature_proof(self):
        self.returncode = 1
        self.summary_changes = {"verdict": "base-as-expected", "counts": {"passed": 1, "failed": 1, "skipped": 0},
                                "assertions": [{"id": "B", "platform": "windows", "ok": False},
                                               {"id": "C", "platform": "linux", "ok": True}]}
        with self.assertRaisesRegex(ValueError, "expected base"):
            coordinated.Session(self.config).run(dataclasses.replace(self.ctx, label="base"))

    def test_missing_native_platform_fails_before_any_fixture_or_device_actions(self):
        for platform in ("macos", "android", "ios"):
            with self.subTest(platform=platform):
                with self.assertRaisesRegex(ValueError, "native.*" + platform):
                    coordinated.Session(self.config).run(dataclasses.replace(self.ctx, platform=platform))
        self.assertEqual(self.invocations, [])

    def test_manifest_cannot_borrow_evidence_outside_run(self):
        self.manifest_changes = {"platforms": {"windows": {"recording": "../unrelated.mp4",
                                   "steps": "windows/steps.jsonl", "seconds": 8, "binary_sha256": DIGEST}}}
        with self.assertRaisesRegex(ValueError, "outside|missing platform"):
            coordinated.Session(self.config).run(self.ctx)

    def test_missing_or_replaced_platform_recording_never_passes(self):
        self.manifest_changes = {"platforms": {}}
        with self.assertRaisesRegex(ValueError, "missing platform"):
            coordinated.Session(self.config).run(self.ctx)

    def test_symlinked_platform_folder_cannot_borrow_another_runs_video(self):
        def execute(command, env, log, timeout):
            code = self.execute(command, env, log, timeout)
            out = Path(command[command.index("--out") + 1])
            borrowed = self.root / "borrowed"
            (out / "windows").rename(borrowed)
            try:
                (out / "windows").symlink_to(borrowed, target_is_directory=True)
            except OSError:
                self.skipTest("symbolic links unavailable on this host")
            return code
        self.invoke.side_effect = execute
        with self.assertRaisesRegex(ValueError, "outside"):
            coordinated.Session(self.config).run(self.ctx)

    def test_timeout_still_cleans_up_and_does_not_retry_for_second_platform(self):
        self.invoke.side_effect = subprocess.TimeoutExpired("coordinator", 60)
        session = coordinated.Session(self.config)
        for platform in ("windows", "linux"):
            with self.assertRaises(subprocess.TimeoutExpired):
                session.run(dataclasses.replace(self.ctx, platform=platform))
        self.assertEqual(len(self.cleanups), 1)
        self.assertEqual(self.invoke.call_count, 1)

    def test_cleanup_failure_cannot_certify_a_run(self):
        self.cleanup.side_effect = RuntimeError("owned fixture cleanup failed")
        with self.assertRaisesRegex(RuntimeError, "cleanup failed"):
            coordinated.Session(self.config).run(self.ctx)

    def test_unpinned_or_unbounded_config_is_rejected(self):
        for change in ({"server_sha": "main"}, {"timeout_seconds": 0}, {"timeout_seconds": 14401},
                       {"ports": {"pg": 1, "http": 1, "proxy": 2, "oidc": 3}}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                coordinated.Session(dict(self.config, **change))


class LabDispatch(unittest.TestCase):
    def test_coordinated_scenario_never_reaches_single_app_adapter(self):
        with tempfile.TemporaryDirectory() as out, mock.patch.object(lab, "load_adapter", side_effect=AssertionError("single app adapter used")), \
             mock.patch.object(lab, "repo_name", return_value="TinyComms/tinyclient"), \
             mock.patch.dict(os.environ, {"E2E_DEVICE_SYNC_CONFIG": ""}):
            result = lab.one_run("windows", "branch", SHA, SHA, SCENARIO, "scenario", "src.tar", out, lambda s: None)
        self.assertTrue(any("E2E_DEVICE_SYNC_CONFIG" in p for p in result["problems"]), result)

    def test_passing_coordinator_ledger_cannot_bypass_video_validation(self):
        with tempfile.TemporaryDirectory() as out:
            raw, steps = Path(out) / "invalid.mp4", Path(out) / "steps.jsonl"
            raw.write_bytes(b"not a video")
            steps.write_text('{"t":1,"step":"linked","ok":true}\n')
            outcome = common.Outcome(True, str(raw), str(steps), 8, binary_sha256=DIGEST)
            with mock.patch.object(lab.coordinated, "run", return_value=outcome), \
                 mock.patch.object(lab, "load_adapter", side_effect=AssertionError("single app adapter used")), \
                 mock.patch.object(lab, "repo_name", return_value="TinyComms/tinyclient"):
                result = lab.one_run("windows", "branch", SHA, SHA, SCENARIO, "scenario", "src.tar", out, lambda s: None)
            self.assertIn("the recording does not play", result["problems"])
            self.assertEqual(lab.judge(SCENARIO, "windows", None, result)[0], "failure")


if __name__ == "__main__":
    unittest.main()
