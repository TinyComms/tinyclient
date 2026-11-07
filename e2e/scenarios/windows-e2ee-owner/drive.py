#!/usr/bin/env python3
"""Windows E2EE private_dir owner-access component proof.

Runs exact chatd tests from E2E_SRC_DIR (or TINYCLIENT_SOURCE override).
Requires a prepared administrators-owned fixture and a limited Windows token.
This is component permission evidence, not native GUI certification.
"""

import json
import os
import re
import subprocess
import sys
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
FAILED = False
STEPS = ""


def now():
    return time.time() - T0


def step(words, ok):
    global FAILED
    if not ok:
        FAILED = True
    line = '{"t": %s, "step": "%s", "ok": %s}' % (
        round(now(), 1),
        words.replace('"', "'"),
        "true" if ok else "false",
    )
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def run_test(cargo, source, env, name, log_name, ignored=False):
    cmd = [cargo, "test", "--locked", "-p", "chatd", "--lib", name,
           "--", "--exact", "--nocapture"]
    if ignored:
        cmd.append("--ignored")
    result = subprocess.run(cmd, cwd=source, env=env, capture_output=True,
                            text=True, timeout=600)
    output = result.stdout + result.stderr
    with open(os.path.join(OUT, log_name), "w", encoding="utf-8") as log:
        log.write(output)
    return result.returncode == 0 and all(
        re.search(pattern, output, re.M) for pattern in (
            r"^running 1 test$",
            r"^test " + re.escape(name) + r" \.\.\. ok$",
            r"^test result: ok\. 1 passed; 0 failed; 0 ignored;",
        ))


def fixture_is_limited_and_admin_owned(fixture):
    script = r'''
$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'
$inputValue=[Console]::In.ReadToEnd() | ConvertFrom-Json
$identity=[Security.Principal.WindowsIdentity]::GetCurrent()
$principal=[Security.Principal.WindowsPrincipal]::new($identity)
$acl=Get-Acl -LiteralPath $inputValue.fixture
$owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
@{limited=(!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator));
  admin_owned=($owner -eq 'S-1-5-32-544');
  user_is_not_system=($identity.User.Value -ne 'S-1-5-18')} | ConvertTo-Json -Compress
'''
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script],
        input=json.dumps({"fixture": fixture}), capture_output=True, text=True, timeout=20)
    if result.returncode:
        return False
    try:
        evidence = json.loads(result.stdout)
    except ValueError:
        return False
    with open(os.path.join(OUT, "fixture-prerequisites.json"), "w", encoding="utf-8") as log:
        json.dump(evidence, log)
    return all(evidence.get(key) is True for key in
               ("limited", "admin_owned", "user_is_not_system"))


def main():
    global STEPS
    if sys.platform != "win32":
        print("windows-only component proof", file=sys.stderr)
        return 1
    if not OUT:
        print("E2E_OUT is required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()

    cargo = os.environ.get("CARGO", "cargo")
    manifest_root = os.environ.get("TINYCLIENT_SOURCE") or os.environ.get("E2E_SRC_DIR")
    if not manifest_root:
        # Lab checkout root: walk up from this scenario folder to the crate root.
        here = os.path.abspath(os.path.dirname(__file__))
        manifest_root = os.path.abspath(os.path.join(here, "..", "..", ".."))
    if not os.path.isfile(os.path.join(manifest_root, "crates", "chatd", "Cargo.toml")):
        step("source checkout contains the chatd manifest", False)
        return 1
    env = os.environ.copy()
    if "TINYCLIENT_DIR" not in env:
        data = os.path.join(OUT, "data")
        os.makedirs(data, exist_ok=True)
        env["TINYCLIENT_DIR"] = data
    mediad = env.get("TINYCLIENT_TEST_MEDIAD") or os.path.join(
        os.environ.get("LOCALAPPDATA", ""),
        "Programs",
        "tinyclient",
        "mediad.exe",
    )
    if os.path.isfile(mediad):
        env["TINYCLIENT_TEST_MEDIAD"] = mediad

    fixture = env.get("TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE", "").strip()
    if not fixture or not os.path.isabs(fixture) or not os.path.isdir(fixture):
        step("prepared administrator-owned fixture is required", False)
        return 1
    with open(os.path.join(fixture, "marker.bin"), "rb") as marker:
        if marker.read() != bytes((1, 2, 3, 4, 5, 9, 9, 9)):
            step("fixture has the dedicated permission-test marker", False)
            return 1
    if not fixture_is_limited_and_admin_owned(fixture):
        step("limited non-system token and administrator-owned fixture are required", False)
        return 1
    env["TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE"] = fixture
    step("process-user SID protected DACL and FileStore persist", run_test(
        cargo, manifest_root, env,
        "adapters::tiny::e2ee::tests::the_folder_is_the_owners_alone", "unit-acl.log"))
    step("administrators-owned fixture keeps bytes and FileStore persist", run_test(
        cargo, manifest_root, env,
        "adapters::tiny::e2ee::tests::administrators_owned_fixture_keeps_user_bytes_and_persist",
        "fixture-acl.log", ignored=True))

    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
