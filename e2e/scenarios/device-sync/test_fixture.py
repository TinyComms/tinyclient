import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(os.name == "nt", "Windows fixture host")
class DatabaseOwnershipTest(unittest.TestCase):
    def cleanup(self, scenario, state):
        root = Path(__file__).resolve().parents[3]
        fixture = root / "e2e/scenarios/device-sync/server/dl-server.ps1"
        owned = str(Path(os.environ["LOCALAPPDATA"]) / "tiny-e2e/test-fixture-ownership-no-processes")
        labels = {"tiny.e2e.scenario": scenario, "tiny.e2e.state": owned if state == "owned" else "another fixture"}
        record = json.dumps([{"Config": {"Labels": labels}}]).replace("'", "''")
        quote = lambda text: "'" + str(text).replace("'", "''") + "'"
        command = """
function docker {
    if ($args[0] -eq 'container' -and $args[1] -eq 'inspect') {
        $global:LASTEXITCODE=0
        '%s'
    } elseif ($args[0] -eq 'rm' -and $args[1] -eq '-f' -and $args[2] -eq 'fixture-ownership-unit-test') {
        $global:LASTEXITCODE=0
        Write-Host 'OWNED_CONTAINER_REMOVED'
    } else { throw 'UNEXPECTED DOCKER OPERATION' }
}
& %s down -TinyServerDir %s -State %s -PgContainer 'fixture-ownership-unit-test'
""" % (record, quote(fixture), quote(root), quote(owned))
        return subprocess.run(["pwsh", "-NoProfile", "-Command", command], capture_output=True, text=True, timeout=30)

    def test_refuses_a_container_from_another_scenario(self):
        result = self.cleanup("another-scenario", "owned")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not owned by this fixture", result.stderr)
        self.assertNotIn("OWNED_CONTAINER_REMOVED", result.stdout)

    def test_refuses_same_scenario_with_different_state_directory(self):
        result = self.cleanup("device-sync", "another")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("OWNED_CONTAINER_REMOVED", result.stdout)

    def test_only_matching_scenario_and_state_may_remove_container(self):
        result = self.cleanup("device-sync", "owned")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("OWNED_CONTAINER_REMOVED", result.stdout)


@unittest.skipUnless(os.name == "nt", "Windows fixture process identity")
class ProcessOwnershipTest(unittest.TestCase):
    def invoke(self, state, child, change):
        script = Path(__file__).parent / "server/dl-server.ps1"
        quote = lambda value: "'" + str(value).replace("'", "''") + "'"
        # Load only these production functions via PowerShell's parser, never
        # invoke a fixture action or inspect any pre-existing state directory.
        command = """
$ErrorActionPreference='Stop'
$tokens=$null; $errors=$null
$ast=[System.Management.Automation.Language.Parser]::ParseFile(%s,[ref]$tokens,[ref]$errors)
$ast.FindAll({param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -in @('Save-Pid','Stop-Pids')},$true) | ForEach-Object { Invoke-Expression $_.Extent.Text }
$PidFile=%s
Save-Pid 'owned' %d
Write-Output ('IDENTITY=' + (Get-Content $PidFile -Raw))
%s
Stop-Pids
""" % (quote(script), quote(Path(state) / "pids.json"), child.pid, change)
        return subprocess.run(["pwsh", "-NoProfile", "-Command", command], capture_output=True, text=True, timeout=30)

    def test_changed_creation_identity_is_not_stopped(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        try:
            with tempfile.TemporaryDirectory() as state:
                changed = self.invoke(state, child, "$r=Get-Content $PidFile -Raw|ConvertFrom-Json; $r.owned.creation_ticks='0'; $r|ConvertTo-Json -Depth 5|Set-Content $PidFile")
                identity = json.loads(changed.stdout.split("IDENTITY=", 1)[1])
                self.assertIsInstance(identity["owned"], dict, "fixture stored a bare PID without native identity")
                self.assertNotEqual(changed.returncode, 0, changed.stdout)
                self.assertIsNone(child.poll())
                matched = self.invoke(state, child, "")
                self.assertEqual(matched.returncode, 0, matched.stderr)
                child.wait(timeout=10)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)

    def test_live_legacy_bare_pid_is_not_stopped(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        try:
            with tempfile.TemporaryDirectory() as state:
                legacy = self.invoke(state, child, "@{owned=%d}|ConvertTo-Json|Set-Content $PidFile" % child.pid)
                self.assertNotEqual(legacy.returncode, 0, legacy.stdout)
                self.assertIsNone(child.poll())
        finally:
            child.kill()
            child.wait(timeout=10)
