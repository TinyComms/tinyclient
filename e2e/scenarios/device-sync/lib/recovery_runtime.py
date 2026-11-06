"""Recovery-run safety and public evidence helpers; no app lifecycle operations.

Directory/device lists are ordinary directory observations, never MLS roster proof.
The orchestrator's evidence cannot certify a native action or an entire platform.
"""
import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import time
import uuid
from urllib.parse import urlsplit


def load_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def atomic_json(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + ".new-" + uuid.uuid4().hex)
    try:
        with temporary.open("x", encoding="utf-8") as output:
            json.dump(value, output, indent=2)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def reserve_namespace(parent, name):
    if not re.fullmatch(r"recovery-[A-Za-z0-9_-]+", name):
        raise ValueError("a new recovery-* namespace is required")
    path = Path(parent).resolve() / name
    path.mkdir(exist_ok=False)
    atomic_json(path / "recovery-owned.json", {"schema": 1, "state": str(path),
                "created_at": time.time(), "id": str(uuid.uuid4())})
    return path


def fixture_sessions(seed):
    """Create unique wrapping keys, not device identity keys. Caller keeps result private.

    Scratch TINYCLIENT_DIR disables Credential Manager. Saved.e2ee supplies wrapping
    keys before process-only scratch_secrets(); preserving these exact per-device
    sessions permits a fixture-key restart test, not a production keyring claim.
    Never overwrite earlier fixture keys or share any wrapping key between devices.
    """
    result = copy.deepcopy(seed)
    sessions = result.get("sessions")
    if not isinstance(sessions, dict) or not sessions:
        raise ValueError("seed sessions missing")
    for session in sessions.values():
        if not isinstance(session, dict) or "e2ee" in session:
            raise ValueError("only untouched new seed sessions may be provisioned")
    for session in sessions.values():
        session["e2ee"] = {kind: base64.b64encode(secrets.token_bytes(32)).decode()
                           for kind in ("state", "cache")}
    return result


def arm_gate(control, conversation, devices, *, seconds=120, gate_id=None):
    for value in [conversation, *devices]:
        if str(uuid.UUID(value)) != value:
            raise ValueError("gate scope requires canonical public IDs")
    if len(devices) != 2 or len(set(devices)) != 2 or not 0 < seconds <= 120:
        raise ValueError("exactly two original readers and a bounded gate required")
    gate_id = gate_id or str(uuid.uuid4())
    if str(uuid.UUID(gate_id)) != gate_id:
        raise ValueError("gate ID must be canonical")
    mode = load_json(control) if Path(control).exists() else {}
    if "handshake_gate" in mode:
        raise ValueError("existing gate must be preserved; use a separately owned fixture")
    gate = dict(id=gate_id, conversation_id=conversation, device_ids=devices,
                state="paused", expires_at=time.time() + seconds, max_wait_seconds=seconds)
    atomic_json(control, dict(mode, handshake_gate=gate))
    return gate


def release_gate(control, gate):
    mode = load_json(control)
    if mode.get("handshake_gate") != gate or gate.get("state") != "paused":
        raise ValueError("release refuses changed gate identity/scope/deadline")
    atomic_json(control, dict(mode, handshake_gate=dict(gate, state="released")))


def gate_readers(rows, gate, event):
    relevant = [row for row in rows if row.get("gate_id") == gate["id"]]
    if any(row.get("event") in ("handshake_gate_timeout", "handshake_gate_capacity") for row in relevant):
        raise ValueError("gate expired or exceeded capacity: preserve attempt as failed")
    readers = {row.get("device_id") for row in relevant
               if row.get("event") == "handshake_gate_" + event
               and row.get("conversation_id") == gate["conversation_id"]}
    if readers != set(gate["device_ids"]):
        raise ValueError("both exact original readers have not reached gate " + event)
    return readers


def assert_stalled(before, after, blocking_seq):
    if set(before) != {"a", "bea"} or set(after) != set(before):
        raise ValueError("both original reader snapshots required")
    for name, original in before.items():
        cursor = original.get("cursor")
        current = after[name].get("cursor")
        if (type(cursor) is not int or type(current) is not int
                or type(blocking_seq) is not int or current != cursor or cursor >= blocking_seq):
            raise ValueError("authenticated feed cursor advanced or did not precede historical Add")
    return True


PHASES = ("prepared", "armed", "held", "nested", "revoked", "released", "stalled", "repair-observed")
GAPS = ("native-desktop-repair", "native-desktop-fresh-text-and-picture", "old-x-y-actual-ciphertext-decryption",
        "fresh-sibling-current-welcome", "real-android-native-ui", "fixture-key-restart", "recording-validation",
        "chosen-marker-server-scan", "base-red-recording")


class Journal:
    def __init__(self, directory):
        self.directory = Path(directory)
        self.path = self.directory / "recovery-journal.json"
        self.data = load_json(self.path) if self.path.exists() else {"schema": 1, "steps": []}

    def require(self, phase):
        expected = PHASES[len(self.data["steps"])] if len(self.data["steps"]) < len(PHASES) else None
        if phase != expected:
            raise ValueError("expected phase " + str(expected) + "; never overwrite or skip evidence")

    def record(self, phase, public):
        self.require(phase)
        self.data["steps"].append({"phase": phase, "at": time.time(), "public": public})
        atomic_json(self.path, self.data)

    def step(self, phase):
        return next(row["public"] for row in self.data["steps"] if row["phase"] == phase)

    def summary(self):
        return {"release": False, "native_proof": False, "execution": "recovery-ipc-observer",
                "completed_phases": [row["phase"] for row in self.data["steps"]], "gaps": list(GAPS)}


def fence_cases(conversation, old_generation, captured):
    """Replay real captured public request bodies; a dummy body can fail before fencing.

    Trees upload inside POST /commit. GET group info is the proposal-issuing
    route, whereas explicit historical handshake/tree reads issue no proposals.
    The caller separately establishes old edit message ownership via server rows.
    """
    root = f"/v1/e2ee/conversations/{conversation}"
    routes = {"create": ("POST", root + "/group"), "commit": ("POST", root + "/commit"),
              "tree": ("POST", root + "/commit"), "message": ("POST", f"/v1/conversations/{conversation}/messages"),
              "edit": ("POST", f"/v1/conversations/{conversation}/messages"),
              "reaction": ("POST", root + "/reactions"), "proposal": ("GET", root)}
    if set(captured) != set(routes) or type(old_generation) is not int or old_generation < 0:
        raise ValueError("all seven actual captured routes and old generation are required")
    for kind, item in captured.items():
        expected = routes.get(kind)
        if (item.get("method"), item.get("path")) != expected:
            raise ValueError("captured route escaped owned conversation")
        body = item.get("body")
        if kind != "proposal" and not isinstance(body, dict):
            raise ValueError("captured request body missing")
        if kind == "tree" and not body.get("ratchet_tree"):
            raise ValueError("tree fence must replay the actual commit/tree upload")
        if kind == "edit" and (body.get("kind") != "edit" or not body.get("target_id")):
            raise ValueError("encrypted edit must name its actual owned target")
    cases = []
    for kind in ("create", "commit", "tree", "message", "edit", "reaction", "proposal"):
        for generation in (None, old_generation):
            item = copy.deepcopy(captured[kind])
            item.update(kind=kind, expected_status=409, expected_error="generation_changed")
            if kind == "proposal":
                item["path"] += "" if generation is None else f"?generation={generation}"
            else:
                item["body"].pop("generation", None)
                if generation is not None:
                    item["body"]["generation"] = generation
            cases.append(item)
    return cases


def assert_send_refused(client_id, rc, answer, before, after):
    if not isinstance(answer, dict):
        raise ValueError("bounded terminal application error required; timeout is inconclusive")
    expected = "A device in this conversation isn't approved by its owner, so nothing was sent."
    failed = rc != 0 and answer.get("op") == "error" and answer.get("detail") == expected
    if not failed:
        raise ValueError("application did not report the expected device-approval refusal; other errors are inconclusive")
    old_ids = {row["id"] for row in before}
    if any(row.get("client_id") == client_id or row.get("id") not in old_ids for row in after):
        raise ValueError("server accepted ciphertext during the refused-send observation")
    return True


def assert_generation_fence(status, body):
    if status != 409 or not isinstance(body, dict) or body.get("error") != "generation_changed":
        raise ValueError("real route did not return 409 generation_changed")
    return True


def assert_authored_ciphertext(row, conversation, device, generation):
    if (not isinstance(row, dict) or not row.get("id") or row.get("e2ee") is not True
            or not row.get("ciphertext") or row.get("device_id") != device
            or row.get("generation") != generation
            or row.get("group_id") != conversation):
        raise ValueError("join control must be actual accepted ciphertext from the named device/generation")
    return True


def artifact_hash(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def validate_manifest(manifest):
    state = Path(manifest["state"]).resolve()
    if not re.fullmatch(r"recovery-[A-Za-z0-9_-]+", state.name):
        raise ValueError("only an explicitly reserved fresh recovery-* fixture may attach")
    ownership = load_json(state / "recovery-owned.json")
    if ownership.get("schema") != 1 or ownership.get("state") != str(state):
        raise ValueError("fixture ownership identity mismatch")
    if manifest.get("key_persistence") != "unique-fixture-wrapping-keys":
        raise ValueError("scratch memory-key fixtures cannot prove persistence")
    sources = manifest.get("sources", {})
    if any(not re.fullmatch(r"[0-9a-f]{40}", sources.get(kind, "")) for kind in ("client", "server")):
        raise ValueError("exact committed client/server source markers required")
    for key in ("base_url", "observer_url"):
        parsed = urlsplit(manifest[key])
        if (parsed.scheme != "http" or parsed.hostname not in ("localhost", "127.0.0.1", "::1")
                or not parsed.port or parsed.path or parsed.username or parsed.query or parsed.fragment):
            raise ValueError("attach observer requires exact loopback fixture URLs")
    for key in ("seed", "proxy_control", "wire"):
        path = Path(manifest[key]).resolve()
        if not path.is_relative_to(state):
            raise ValueError("fixture path escaped owned namespace")
    if set(manifest["devices"]) != {"a", "bea", "x", "y"}:
        raise ValueError("four exact owned participants required")
    if len(set(manifest["devices"].values())) != 4:
        raise ValueError("participants must have separate profiles")
    for name in manifest["devices"].values():
        if not re.fullmatch(r"[A-Za-z0-9_-]+", name):
            raise ValueError("unsafe profile name")
    for artifact in manifest["artifacts"].values():
        if artifact_hash(artifact["path"]) != artifact["sha256"]:
            raise ValueError("artifact bytes differ from exact source/build inventory")
    if not {"chatd", "tinyclient", "dlctl", "tinyserver"}.issubset(manifest["artifacts"]):
        raise ValueError("all final-source application/helper artifacts must be pinned")
    return state
