"""Owned device registry. Never stop a process using a bare recycled PID."""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess


def remote_path(path):
    return '"$HOME"/' + shlex.quote(path[2:]) if path.startswith("~/") else shlex.quote(path)


def _read(state):
    root = Path(state).resolve()
    path = root / "owned-devices.json"
    if not path.exists():
        return {"schema": 1, "state": str(root), "windows": [], "linux": []}
    record = json.loads(path.read_text(encoding="utf-8"))
    if record.get("schema") != 1 or record.get("state") != str(root):
        raise ValueError("device ownership registry belongs to another state directory")
    return record


def _save(state, record):
    root = Path(state).resolve()
    root.mkdir(parents=True, exist_ok=True)
    temp = root / "owned-devices.json.new"
    temp.write_text(json.dumps(record, indent=2), encoding="utf-8")
    os.replace(temp, root / "owned-devices.json")


def _windows(action, record):
    # Keep the Process object/handle through validation and Kill(), preventing
    # reuse between the metadata check and the stop operation.
    script = r'''
$ErrorActionPreference='Stop'
$r = [Console]::In.ReadToEnd() | ConvertFrom-Json
$p = Get-Process -Id $r.pid -ErrorAction SilentlyContinue
if (-not $p) { if ($r.action -eq 'inspect') { throw 'owned process exited before identity capture' }; exit 0 }
$actual = @{pid=$p.Id; creation_ticks=$p.StartTime.ToUniversalTime().Ticks.ToString(); executable=$p.Path}
if ($actual.executable -ne $r.executable) { throw 'owned process executable identity mismatch' }
if ($r.action -eq 'inspect') { $actual | ConvertTo-Json -Compress; exit 0 }
if ($actual.creation_ticks -ne $r.creation_ticks) { throw 'owned process creation identity mismatch' }
$p.Kill()
$p.WaitForExit(10000) | Out-Null
'''
    response = subprocess.run(["powershell.exe", "-NoProfile", "-Command", script],
                              input=json.dumps(dict(record, action=action)), capture_output=True,
                              text=True, timeout=30, creationflags=0x08000000)
    if response.returncode:
        raise RuntimeError("owned Windows process identity/cleanup failed: " + response.stderr.strip()[-300:])
    return json.loads(response.stdout) if response.stdout.strip() else None


def register_windows(state, pid, executable):
    identity = _windows("inspect", {"pid": pid, "executable": str(Path(executable).resolve())})
    record = _read(state)
    record["windows"].append(identity)
    _save(state, record)
    return identity


def register_linux(state, name, host, work, app_dir, state_root):
    if host != "tiny-linux" or not re.fullmatch("[A-Za-z0-9_-]+", name):
        raise ValueError("Linux device ownership requires the configured test alias and a safe profile name")
    if not state_root or not state_root.startswith("/tmp/") or ".." in Path(state_root).parts:
        raise ValueError("Linux device ownership requires an isolated /tmp state root")
    entry = dict(name=name, host=host, work=work, app_dir=app_dir, state_root=state_root)
    record = _read(state)
    if entry not in record["linux"]:
        record["linux"].append(entry)
        _save(state, record)


def _remote_stop(entry):
    if entry.get("host") != "tiny-linux" or not re.fullmatch("[A-Za-z0-9_-]+", entry.get("name", "")):
        raise ValueError("invalid Linux ownership entry")
    env = "DL_STATE_ROOT=%s DL_APP_DIR=%s" % (remote_path(entry["state_root"]), remote_path(entry["app_dir"]))
    script = remote_path(entry["work"].rstrip("/") + "/linux-host.sh")
    # stop includes the root script's per-process PID/startticks/exe/UID guard.
    subprocess.run(["ssh", entry["host"], "%s bash %s stop %s" % (env, script, shlex.quote(entry["name"]))],
                   capture_output=True, text=True, timeout=90, check=True)


def cleanup(state):
    record = _read(state)
    errors = []
    done = {"windows": 0, "linux": 0}
    for platform, stop in (("windows", lambda item: _windows("stop", item)), ("linux", _remote_stop)):
        retained = []
        for item in record[platform]:
            try:
                stop(item)
                done[platform] += 1
            except Exception as error:
                errors.append(str(error))
                retained.append(item)
        record[platform] = retained
    _save(state, record)
    if errors:
        raise RuntimeError("owned device cleanup failed: " + "; ".join(errors))
    return done
