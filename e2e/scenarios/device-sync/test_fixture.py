import json
import os
from pathlib import Path
import subprocess
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
