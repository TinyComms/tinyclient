"""Real-process cleanup regressions; run on Linux, never against application processes."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid

HERE = Path(__file__).resolve().parent


@unittest.skipUnless(sys.platform == "linux", "requires Linux process identities and pidfds")
class LinuxLifecycleTest(unittest.TestCase):
    def setUp(self):
        self.name = "guard-" + uuid.uuid4().hex[:12]
        self.profile = Path("/tmp") / ("tiny-dl-" + self.name)
        self.profile.mkdir(mode=0o700)
        self.recorder = Path("/tmp") / ("tiny-dl-rec-" + self.name)
        self.processes = []

    def tearDown(self):
        for process in self.processes:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
        shutil.rmtree(self.profile)
        if self.recorder.exists():
            shutil.rmtree(self.recorder)

    def sleeper(self):
        process = subprocess.Popen(["sleep", "60"])
        self.processes.append(process)
        return process

    def identity(self, process):
        proc = Path("/proc") / str(process.pid)
        fields = (proc / "stat").read_text().rsplit(") ", 1)[1].split()
        return {"pid": process.pid, "uid": proc.stat().st_uid,
                "start_ticks": int(fields[19]), "exe": os.readlink(proc / "exe"),
                "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip()}

    def record(self, profile, process, **changes):
        identity = dict(self.identity(process), **changes)
        (profile / "pids").write_text(str(process.pid) + "\n")
        (profile / "processes.json").write_text(json.dumps({"schema": 1, "processes": [identity]}))

    def stop(self, **extra_env):
        return subprocess.run(["bash", str(HERE / "drivers/linux-host.sh"), "stop", self.name],
                              env=dict(os.environ, **extra_env), capture_output=True, text=True, timeout=8)

    def test_reused_pid_start_identity_is_never_signalled(self):
        process = self.sleeper()
        self.record(self.profile, process, start_ticks=0)
        result = self.stop()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNone(process.poll(), "a mismatched process was signalled")

    def test_executable_mismatch_is_never_signalled(self):
        process = self.sleeper()
        self.record(self.profile, process, exe="/unrelated/application")
        result = self.stop()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNone(process.poll())

    def test_legacy_pid_alone_is_insufficient_ownership(self):
        process = self.sleeper()
        (self.profile / "pids").write_text(str(process.pid) + "\n")
        result = self.stop()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNone(process.poll())

    def test_matching_owned_process_is_stopped(self):
        process = self.sleeper()
        self.record(self.profile, process)
        result = self.stop()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNotNone(process.poll())
        self.assertFalse((self.profile / "pids").exists())

    def test_custom_state_root_does_not_stop_default_profile(self):
        unrelated = self.sleeper()
        self.record(self.profile, unrelated)
        with tempfile.TemporaryDirectory(prefix="tiny-dl-state-") as root:
            profile = Path(root) / self.name
            profile.mkdir(mode=0o700)
            owned = self.sleeper()
            self.record(profile, owned)
            result = self.stop(DL_STATE_ROOT=root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIsNotNone(owned.poll())
            self.assertIsNone(unrelated.poll())

    def test_traversal_name_is_rejected(self):
        result = subprocess.run(["bash", str(HERE / "drivers/linux-host.sh"), "stop", "../escape"],
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)

    def test_recording_executable_mismatch_is_not_signalled(self):
        process = self.sleeper()
        self.recorder.mkdir(mode=0o700)
        self.record(self.recorder, process, exe="/unrelated/recorder")
        (self.recorder / "pids").rename(self.recorder / "rec.pid")
        result = subprocess.run(["bash", str(HERE / "drivers/linux-host.sh"), "rec-stop", self.name],
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(process.poll())

    def test_real_spawn_registration_and_guarded_stop(self):
        process = self.sleeper()
        result = subprocess.run([sys.executable, str(HERE / "drivers/linux-processes.py"), "record",
                                 "--manifest", str(self.profile / "processes.json"),
                                 "--legacy", str(self.profile / "pids"), "--pid", str(process.pid),
                                 "--expected-exe", shutil.which("sleep")], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        record = json.loads((self.profile / "processes.json").read_text())["processes"][0]
        self.assertEqual(record, self.identity(process))
        self.assertEqual(self.stop().returncode, 0)
        self.assertIsNotNone(process.poll())


if __name__ == "__main__":
    unittest.main()
