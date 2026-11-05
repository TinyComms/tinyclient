"""Bounded device-sync fixture shared by the platform proof workers.

The scenario owns the devices; the lab owns its deadline, fixture lifetime and
evidence validation. Configuration is private and never copied to published proof.
"""
import copy
import ipaddress
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import threading
import uuid

import common

PLATFORMS = ("windows", "linux")
NEEDS = {"server-device-link", "server-self-sync", "two-people",
         "device-sync-coordinator", "device-sync-extended"}
_sessions = {}
_sessions_lock = threading.Lock()
# The Linux participant uses one prepared immutable binary folder. Even two
# unrelated platform worker groups must never reset its profiles concurrently.
_fixture_lock = threading.Lock()


def _json(path):
    with open(path, encoding="utf-8-sig") as file:
        return json.load(file)


def _digest(value, length):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{%d}" % length, value)


def _invoke(command, env, log, timeout):
    if os.name != "nt":
        raise ValueError("the device-sync coordinator requires a Windows console session")
    with open(log, "w", encoding="utf-8") as output:
        process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                   stdout=output, stderr=subprocess.STDOUT)
        try:
            return process.wait(timeout=timeout)
        except BaseException:
            # Popen keeps a handle to this exact process. Its owned children are
            # stopped separately through creation-time/executable guarded records.
            process.kill()
            process.wait(timeout=30)
            raise


def _cleanup(scenario_dir, state, container, env, log):
    command = ["pwsh", "-NoProfile", "-File", str(Path(scenario_dir) / "server/dl-server.ps1"),
               "down", "-State", str(state), "-PgContainer", container]
    with open(log, "a", encoding="utf-8") as output:
        try:
            subprocess.run([sys.executable, str(Path(scenario_dir) / "dl.py"), "cleanup", "--state", str(state)],
                           env=env, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT, timeout=180, check=True)
        finally:
            subprocess.run(command, env=env, stdin=subprocess.DEVNULL, stdout=output,
                           stderr=subprocess.STDOUT, timeout=120, check=True)


class Session:
    def __init__(self, config):
        self.config = copy.deepcopy(config)
        self.lock = threading.Lock()
        self.results = {}
        if not _digest(config.get("server_sha"), 40) or not config.get("server_dir"):
            raise ValueError("device-sync requires server_dir and a full committed server_sha")
        timeout = config.get("timeout_seconds")
        if type(timeout) is not int or not 1 <= timeout <= 14400:
            raise ValueError("device-sync timeout_seconds must be in 1..14400")
        ports = config.get("ports", {})
        if set(ports) != {"pg", "http", "proxy", "oidc", "media"} or any(
                type(p) is not int or not 1024 <= p <= 65535 for p in ports.values()) or len(set(ports.values())) != 5:
            raise ValueError("device-sync requires five distinct explicit fixture ports in 1024..65535")
        try:
            ipaddress.ip_address(config.get("advertise", ""))
        except ValueError as error:
            raise ValueError("device-sync requires an explicit advertise IP from private configuration") from error

    def run(self, ctx):
        if ctx.platform not in PLATFORMS:
            raise ValueError("device-sync needs a native %s coordinator participant; no native driver is registered" % ctx.platform)
        if ctx.scenario.get("id") != "device-sync" or ctx.scenario.get("execution") != "coordinated-ipc":
            raise ValueError("unsupported coordinated scenario")
        if set(ctx.scenario.get("needs", [])) != NEEDS or ctx.scenario.get("required_args") != ["--extended"]:
            raise ValueError("device-sync descriptor must require its complete extended fixtures")
        if ctx.scenario.get("release"):
            raise ValueError("device-sync has no complete native platform proof and cannot certify a release")
        root = Path(ctx.out_dir).resolve().parents[2]
        key = (str(root), ctx.app_sha, ctx.label, ctx.scenario_sha)
        with self.lock:
            if key not in self.results:
                try:
                    with _fixture_lock:
                        self.results[key] = self._execute(ctx, root)
                except BaseException as error:
                    self.results[key] = error
            result = self.results[key]
            if isinstance(result, BaseException):
                raise result
            return copy.deepcopy(result[ctx.platform])

    def _execute(self, ctx, root):
        if not _digest(ctx.app_sha, 40):
            raise ValueError("device-sync requires a full client source SHA")
        app_dir = self.config.get("app_dirs", {}).get(ctx.app_sha)
        if not app_dir:
            raise ValueError("device-sync has no pinned Windows app_dir for this exact client SHA")
        linux_app_dir = self.config.get("linux_app_dirs", {}).get(ctx.app_sha)
        if not linux_app_dir:
            raise ValueError("device-sync has no pinned Linux app_dir for this exact client SHA")
        marker = Path(app_dir) / "SOURCE_SHA"
        if not marker.is_file() or marker.read_text(encoding="utf-8-sig").strip() != ctx.app_sha:
            raise ValueError("device-sync binary source marker does not match this exact client SHA")
        # A fresh UUID isolates both fixture state and evidence, even if the caller
        # reuses --out. Never attach to or reset an earlier fixture.
        rid = "device-sync-%s-%s-%s" % (ctx.label, ctx.app_sha[:12], uuid.uuid4().hex[:12])
        run_root = root / "coordinated" / rid
        run_root.mkdir(parents=True)
        source = run_root / "source"
        source.mkdir()
        with tarfile.open(ctx.source_tar) as archive:
            archive.extractall(source, filter="data")
        staged_scenario = source / "e2e/scenarios/device-sync"
        if staged_scenario.exists():
            shutil.rmtree(staged_scenario)
        shutil.copytree(ctx.scenario_dir, staged_scenario)
        # Old app commits can predate the lab. This is recorder infrastructure
        # from the running lab, not part of the product build under test.
        (source / "e2e/lab").mkdir(parents=True, exist_ok=True)
        shutil.copyfile(Path(__file__).parent / "common.py", source / "e2e/lab/common.py")
        private_base = Path(os.environ.get("LOCALAPPDATA") or os.path.expanduser("~/.cache")) / "tiny-e2e/lab-fixtures"
        state, evidence = private_base / rid, run_root / "evidence"
        container = "tiny-lab-" + rid
        env = dict(os.environ, DL_STATE=str(state), E2E_SCENARIO_SHA=ctx.scenario_sha)
        if self.config.get("advertise"):
            env["DL_ADVERTISE"] = self.config["advertise"]
        command = [sys.executable, str(staged_scenario / "dl.py"), "run",
                   "--app-dir", app_dir, "--client-sha", ctx.app_sha,
                   "--linux-app-dir", linux_app_dir, "--linux-state-dir", "/tmp/dl-" + uuid.uuid4().hex[:12],
                   "--server-dir", self.config["server_dir"], "--server-ref", self.config["server_sha"],
                   "--devices", "desk_b,linux", "--history", "6", "--link-ttl", "60",
                   "--cases", "reject,mitm,tamper,reuse,expired", "--extended",
                   "--mode", "legacy" if ctx.label == "base" else "v1",
                   "--pg-container", container, "--out", str(evidence)]
        for name, port in self.config["ports"].items():
            command += ["--%s-port" % name, str(port)]
        log = str(run_root / "coordinator.log")
        ctx.log("starting fresh coordinated IPC fixture for %s@%s; native desktop evidence only" % (ctx.label, ctx.app_sha[:12]))
        try:
            code = _invoke(command, env, log, self.config["timeout_seconds"])
        finally:
            _cleanup(str(staged_scenario), state, container, env, str(run_root / "cleanup.log"))
        return self._outcomes(ctx, evidence, code)

    def _outcomes(self, ctx, evidence, code):
        manifest, summary = _json(evidence / "lab-result.json"), _json(evidence / "summary.json")
        if (manifest.get("schema") != 1 or manifest.get("execution") != "coordinated-ipc"
                or manifest.get("client_sha") != ctx.app_sha or summary.get("client_sha") != ctx.app_sha
                or summary.get("scenario_sha") != ctx.scenario_sha
                or manifest.get("server_sha") != self.config["server_sha"]
                or summary.get("server", {}).get("tinyserver_sha") != self.config["server_sha"]):
            raise ValueError("coordinator evidence provenance does not match the requested client/server sources")
        verdict = summary.get("verdict")
        if manifest.get("completed") is not True or verdict not in ("pass", "base-as-expected", "fail"):
            raise ValueError("coordinator run is incomplete: %s" % verdict)
        if code not in (0, 1) or (code == 0) != (verdict == "pass"):
            raise ValueError("coordinator exit code contradicts the assertion ledger")
        if ctx.label == "base" and verdict != "base-as-expected":
            raise ValueError("base must fail the expected feature assertions; verdict was %s" % verdict)
        assertions = summary.get("assertions", [])
        if not assertions or any(not any(a.get("platform") == p and type(a.get("ok")) is bool for a in assertions) for p in PLATFORMS):
            raise ValueError("coordinator assertion ledger is missing a required platform")
        failed = [a for a in assertions if a.get("ok") is False]
        counts = summary.get("counts", {})
        if (any(a.get("ok") is not None and type(a.get("ok")) is not bool for a in assertions)
                or counts.get("passed") != sum(a.get("ok") is True for a in assertions)
                or counts.get("failed") != len(failed)
                or counts.get("skipped") != sum(a.get("ok") is None for a in assertions)
                or (verdict == "pass" and failed)):
            raise ValueError("coordinator verdict/counts contradict the assertion ledger")
        if ctx.label == "base" and (not failed or any(a.get("expected_on_base") is not False for a in failed)):
            raise ValueError("coordinator failed assertions are not the expected base regression")
        result = {}
        for platform in PLATFORMS:
            entry = manifest.get("platforms", {}).get(platform)
            if not isinstance(entry, dict):
                raise ValueError("coordinator evidence is missing platform %s" % platform)
            files = {}
            for field in ("recording", "steps"):
                relative = entry.get(field)
                if not isinstance(relative, str) or not relative or Path(relative).is_absolute():
                    raise ValueError("coordinator %s must be relative to its own run" % field)
                path = (evidence / relative).resolve()
                if not path.is_relative_to(evidence.resolve()) or not path.is_relative_to((evidence / platform).resolve()):
                    raise ValueError("coordinator evidence is outside its platform run")
                if not path.is_file():
                    raise ValueError("coordinator evidence file is missing: %s" % field)
                files[field] = str(path)
            seconds = entry.get("seconds")
            if type(seconds) not in (int, float) or not math.isfinite(seconds) or seconds <= 0 or not _digest(entry.get("binary_sha256"), 64):
                raise ValueError("coordinator recording timing or native binary digest is invalid")
            if type(entry.get("vfr", False)) is not bool:
                raise ValueError("coordinator vfr flag must be boolean")
            result[platform] = common.Outcome(verdict == "pass", **files, seconds=seconds,
                                             binary_sha256=entry["binary_sha256"], vfr=entry.get("vfr", False),
                                             note="coordinated IPC with real desktop windows; GUI link clicks are not certified")
        return result


def run(ctx):
    path = os.environ.get("E2E_DEVICE_SYNC_CONFIG")
    if not path:
        raise ValueError("set E2E_DEVICE_SYNC_CONFIG to a private pinned coordinator configuration")
    # One immutable configuration snapshot for both platform worker groups.
    root = str(Path(ctx.out_dir).resolve().parents[2])
    with _sessions_lock:
        if root not in _sessions:
            _sessions[root] = Session(_json(path))
        session = _sessions[root]
    return session.run(ctx)
