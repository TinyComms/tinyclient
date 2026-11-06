"""Attach-only recovery coordinator. Starts/stops no app, recorder or server.

Every mutating phase is explicit and one-shot. Native link approval, repair,
positive sends, Android UI and restart remain external recorded checkpoints.
See RECOVERY-RUN.md. Summary is always release:false, native_proof:false.
"""
import argparse
import base64
import json
from pathlib import Path
import sys
import time
import urllib.error
import urllib.request
import uuid

from lib import recovery_runtime as rt
from lib.attachments import verify_picture
from lib.devices import WinLocal
from lib.recovery import inspect_repair_response


def request(base, token, method, path, body=None):
    """Private credential stays in memory; never include URL/header/body in errors."""
    if not path.startswith("/v1/") or "#" in path or "\r" in path or "\n" in path:
        raise ValueError("invalid fixture route")
    req = urllib.request.Request(base + path, method=method,
          data=None if body is None else json.dumps(body).encode(),
          headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
    try:
        response = urllib.request.urlopen(req, timeout=15)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read()
        try:
            answer = json.loads(raw) if raw else None
        except ValueError:
            answer = None
        return response.status, answer


def message_rows(answer):
    if isinstance(answer, list):
        rows = answer
    elif isinstance(answer, dict) and isinstance(answer.get("messages"), list):
        rows = answer["messages"]
    else:
        raise ValueError("unrecognized real server message page")
    if len(rows) >= 100:
        raise ValueError("bounded fresh fixture requires fewer than100 messages; a full page is inconclusive")
    return rows


class RecoveryRun:
    def __init__(self, manifest, out, device_factory=WinLocal):
        self.manifest = manifest
        self.state = rt.validate_manifest(manifest)
        self.out = Path(out)
        self.journal = rt.Journal(self.out)
        self.conv = str(uuid.UUID(manifest["conversation_id"]))
        self.root = f"/v1/e2ee/conversations/{self.conv}"
        self.seed = rt.load_json(manifest["seed"])
        self.devices = {}
        wrapping_keys = []
        for role, name in manifest["devices"].items():
            session = self.seed["sessions"][name]
            keys = session.get("e2ee", {})
            if set(keys) != {"state", "cache"}:
                raise ValueError("per-device durable fixture wrapping keys missing")
            for value in keys.values():
                if len(base64.b64decode(value, validate=True)) != 32:
                    raise ValueError("invalid private fixture wrapping-key size")
                wrapping_keys.append(value)
            self.devices[role] = device_factory(name=name,
                session=dict(session, base_url=manifest["base_url"]),
                app_dir=str(Path(manifest["artifacts"]["chatd"]["path"]).parent),
                dlctl=manifest["artifacts"]["dlctl"]["path"], state=str(self.state),
                label=role, gui=False, log=lambda *args: None)
        if len(set(wrapping_keys)) != 8:
            raise ValueError("fixture wrapping keys must never be shared between devices")

    def server(self, method, path, body=None, role="a"):
        name = self.manifest["devices"][role]
        return request(self.manifest["observer_url"], self.seed["sessions"][name]["token"], method, path, body)

    def get(self, path, role="a"):
        status, answer = self.server("GET", path, role=role)
        if status != 200:
            raise ValueError("owned fixture observation failed with HTTP " + str(status))
        return answer

    def wire(self):
        path = Path(self.manifest["wire"])
        # Ignore only an unterminated final append. A malformed complete record fails.
        data = path.read_bytes() if path.exists() else b""
        return [json.loads(line) for line in data.splitlines(keepends=True) if line.endswith(b"\n")]

    def cursor(self, role):
        profile = Path(self.devices[role].dir).resolve()
        if not profile.is_relative_to(self.state / "devices"):
            raise ValueError("cursor profile escaped owned fixture")
        paths = list(profile.glob("e2ee/*/text/notes.json"))
        if len(paths) != 1:
            raise ValueError("exactly one owned public notes file required")
        value = rt.load_json(paths[0]).get("seqs", {}).get(self.conv, 0)
        if type(value) is not int or value < 0:
            raise ValueError("invalid public generation0 cursor")
        return {"cursor": value}

    def originals(self):
        return {role: self.cursor(role) for role in ("a", "bea")}

    def find_message(self, role, message_id):
        message = next((row for row in (self.devices[role].messages(self.conv) or [])
                        if row.get("id") == message_id), None)
        if not message or message.get("seal") != "encrypted":
            raise ValueError("actual device has not decrypted the required encrypted message")
        return message

    def prepared(self, history):
        self.journal.require("prepared")
        if history.get("conversation_id") != self.conv:
            raise ValueError("baseline belongs to another conversation")
        identities = {}
        for role in ("a", "bea", "x"):
            status = self.devices[role].e2ee_status()
            if not isinstance(status, dict) or not status.get("device_id") or status.get("revoked"):
                raise ValueError("originals and X must already be active approved devices")
            identities[role] = status["device_id"]
            # A fresh X-authored send after its genuine Welcome proves actual
            # decryption, rather than relying on imported/cache-only old history.
            joined = self.find_message(role, history["x_join_message_id"])
            if joined.get("text") != history["x_join_text"]:
                raise ValueError("X join control text differs")
        y_status = self.devices["y"].e2ee_status()
        if (not isinstance(y_status, dict) or not y_status.get("device_id")
                or (y_status.get("device") or {}).get("state") != "awaiting_approval"):
            raise ValueError("Y must be enrolled and awaiting its ordinary approval")
        identities["y"] = y_status["device_id"]
        if len(set(identities.values())) != 4:
            raise ValueError("four distinct device identities required")
        accepted = message_rows(self.get(f"/v1/conversations/{self.conv}/messages?limit=100"))
        x_control = next((row for row in accepted if row.get("id") == history["x_join_message_id"]), None)
        rt.assert_authored_ciphertext(x_control, self.conv, identities["x"], 0)
        saved = []
        for role in ("a", "bea"):
            for expected in history["messages"]:
                actual = self.find_message(role, expected["id"])
                if actual.get("text") != expected["text"]:
                    raise ValueError("saved pre-repair text differs")
            picture = history["picture"]
            actual = self.find_message(role, picture["id"])
            result = verify_picture(self.devices[role], actual, picture)
            if not result["ok"]:
                raise ValueError("actual original-device normalized picture bytes differ")
            saved.append({"role": role, "picture": result})
        metadata = self.get(self.root + "?generation=0")
        if metadata.get("generation") != 0 or metadata.get("group_id") != self.conv:
            raise ValueError("deterministic red starts from an ordinary generation0 group")
        evidence = {"device_ids": identities, "cursors": self.originals(), "metadata": metadata,
                    "history": history, "download_controls": saved,
                    "artifacts": self.manifest["artifacts"], "sources": self.manifest.get("sources", {}),
                    "key_persistence": "unique fixture wrapping keys; not Credential Manager proof"}
        self.journal.record("prepared", evidence)

    def arm(self):
        self.journal.require("armed")
        prepared = self.journal.step("prepared")
        if self.originals() != prepared["cursors"]:
            raise ValueError("readers moved since preparation; preserve this attempt and reassess before arming")
        gate = rt.arm_gate(self.manifest["proxy_control"], self.conv,
                          [prepared["device_ids"][role] for role in ("a", "bea")])
        self.journal.record("armed", {"gate": gate, "cursors": prepared["cursors"]})

    def gate(self):
        return self.journal.step("armed")["gate"]

    def held(self):
        self.journal.require("held")
        rt.gate_readers(self.wire(), self.gate(), "held")
        cursors = self.originals()
        if cursors != self.journal.step("armed")["cursors"]:
            raise ValueError("a reader advanced before its hold")
        self.journal.record("held", {"cursors": cursors, "gate_id": self.gate()["id"]})

    def require_held(self):
        gate = self.gate()
        rt.gate_readers(self.wire(), gate, "held")
        if time.time() >= gate["expires_at"] or rt.load_json(self.manifest["proxy_control"]).get("handshake_gate") != gate:
            raise ValueError("bounded exact gate is no longer held")

    def nested(self, message_id):
        self.journal.require("nested")
        self.require_held()
        y = self.journal.step("prepared")["device_ids"]["y"]
        after = min(row["cursor"] for row in self.journal.step("held")["cursors"].values())
        feed = self.get(f"{self.root}/handshake?generation=0&device_id={y}&after={after}", role="y")
        commits = [row for row in feed if row.get("kind") == "commit"]
        welcomes = [row for row in feed if row.get("kind") == "welcome"]
        if not commits or not welcomes:
            raise ValueError("actual accepted nested commit and Y-targeted Welcome required")
        fresh = self.find_message("y", message_id)
        server_rows = message_rows(self.get(f"/v1/conversations/{self.conv}/messages?limit=100"))
        accepted = next((row for row in server_rows if row.get("id") == message_id), None)
        x = self.journal.step("prepared")["device_ids"]["x"]
        rt.assert_authored_ciphertext(accepted, self.conv, x, 0)
        if accepted.get("epoch", -1) < max(row["epoch"] for row in commits):
            raise ValueError("Y control predates its nested Add")
        self.require_held()
        self.journal.record("nested", {"blocking_seq": max(row["seq"] for row in commits),
                    "feed_coordinates": [{key: row.get(key) for key in ("seq", "kind", "epoch", "generation", "group_id")} for row in feed],
                    "y_decrypted_message_id": fresh["id"], "server_message_id": accepted["id"]})

    def revoked(self):
        self.journal.require("revoked")
        self.require_held()
        identities = self.journal.step("prepared")["device_ids"]
        x = self.devices["x"]
        for role in ("y", "x"):
            self.require_held()
            rc, answer = x.e2ee({"op": "revoke", "device_id": identities[role]}, timeout=20)
            if rc:
                raise ValueError("owned nested revocation failed; preserve attempt")
        directory = self.get(f"/v1/e2ee/users/{x.user_id}/devices")
        rows = directory if isinstance(directory, list) else directory.get("devices", [])
        # Ordinary user_devices is explicitly filtered to revoked_at IS NULL
        # by app_e2ee_user_devices; rows have no invented `approved` field.
        active = {row["device_id"] for row in rows}
        if any(identities[role] in active for role in ("x", "y")):
            raise ValueError("fresh ordinary directory still reports X/Y active")
        if identities["a"] not in active:
            raise ValueError("original owner unexpectedly inactive")
        self.require_held()
        self.journal.record("revoked", {"active_device_ids": sorted(active), "revocation_order": [identities["y"], identities["x"]]})

    def released(self):
        self.journal.require("released")
        self.require_held()
        rt.release_gate(self.manifest["proxy_control"], self.gate())
        # Failures are preserved; this phase never retries release or changes its deadline.
        deadline = time.monotonic() + 5
        while True:
            try:
                rt.gate_readers(self.wire(), self.gate(), "released")
                break
            except ValueError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.05)
        self.journal.record("released", {"gate_id": self.gate()["id"]})

    def stalled(self):
        self.journal.require("stalled")
        rt.gate_readers(self.wire(), self.gate(), "released")
        before = self.journal.step("held")["cursors"]
        blocking = self.journal.step("nested")["blocking_seq"]
        rt.assert_stalled(before, self.originals(), blocking)
        attempts = []
        for role in ("a", "bea"):
            dev = self.devices[role]
            client_id = str(uuid.uuid4())
            path = f"/v1/conversations/{self.conv}/messages?limit=100"
            old = message_rows(self.get(path))
            rc, answer = dev.ipc({"op": "send", "conversation": dev.key(self.conv),
                                  "client_id": client_id, "text": "Owned recovery deadlock probe " + client_id}, timeout=35)
            if rc == 0:
                # Send may enqueue normally and fail asynchronously. Observe the actual
                # terminal outbox state; a pending message/cache delay is not a refusal.
                def failure():
                    row = next((row for row in (dev.messages(self.conv) or []) if row.get("client_id") == client_id), {})
                    state = row.get("state", {})
                    if isinstance(state, dict) and state.get("state") == "failed" and state.get("reason"):
                        return {"op": "error", "detail": state["reason"], "observation": "outbox_failed"}
                terminal = dev.wait_until("bounded actual send refusal", failure, 25)
                if terminal:
                    rc, answer = 4, terminal
            now = message_rows(self.get(path))
            rt.assert_send_refused(client_id, rc, answer, old, now)
            attempts.append({"role": role, "client_id": client_id, "terminal_error": answer,
                             "accepted_before": [row["id"] for row in old], "accepted_after": [row["id"] for row in now]})
        rt.assert_stalled(before, self.originals(), blocking)
        metadata = self.get(self.root + "?generation=0")
        if metadata["epoch"] <= self.journal.step("prepared")["metadata"]["epoch"]:
            raise ValueError("server epoch did not advance through nested Add")
        self.journal.record("stalled", {"cursors": self.originals(), "blocking_seq": blocking,
                    "advanced_server_epoch": metadata["epoch"], "real_send_refusals": attempts,
                    "limit": "Directory lists are not an authenticated MLS roster; retain client logs separately."})

    def repair_observed(self):
        self.journal.require("repair-observed")
        chain = self.get(self.root + "/generations?after=0&limit=100")
        if chain.get("generation") != 1 or len(chain.get("bindings", [])) != 1:
            raise ValueError("exact immediate successor required")
        response = {key: chain[key] for key in ("generation", "group_id", "head_hash")}
        response["binding"] = chain["bindings"][0]
        # Pin was independently observed on the existing group before the gate.
        pinned = self.journal.step("prepared")["metadata"]["external_sender"]
        head = inspect_repair_response(response, pinned)
        if response["binding"]["conversation_id"] != self.conv:
            raise ValueError("binding escaped target conversation")
        self.journal.record("repair-observed", {"public_response": response, "independent_head_check": head,
                    "native_owner_verification": "External recording/outcome required; no IPC action dispatched."})

    def fences(self, captured):
        if "repair-observed" not in self.journal.summary()["completed_phases"]:
            raise ValueError("observe actual repair before replaying retired-generation writes")
        destination = self.out / "generation-fences.json"
        if destination.exists():
            raise ValueError("preserve existing fence attempt; never overwrite it")
        results = []
        for case in rt.fence_cases(self.conv, 0, captured):
            status, body = self.server(case["method"], case["path"], case.get("body"))
            result = {"kind": case["kind"], "path": case["path"], "status": status,
                      "error": body.get("error") if isinstance(body, dict) else None}
            results.append(result)
            rt.atomic_json(destination, {"release": False, "results": results,
                           "limits": "Separate legacy generation0 control, historical reads and equal-epoch captures still required."})
            rt.assert_generation_fence(status, body)


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("action", choices=("reserve", "prepared", "armed", "held", "nested", "revoked", "released", "stalled", "repair-observed", "fences", "summary"))
    result.add_argument("--manifest", type=Path)
    result.add_argument("--out", type=Path)
    result.add_argument("--history", type=Path)
    result.add_argument("--captured", type=Path)
    result.add_argument("--message-id")
    result.add_argument("--parent", type=Path)
    result.add_argument("--name")
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    if args.action == "reserve":
        if not args.parent or not args.name:
            raise ValueError("reserve requires --parent and --name")
        print(json.dumps({"reserved": str(rt.reserve_namespace(args.parent, args.name)), "release": False}))
        return 0
    if not args.out:
        raise ValueError("--out required")
    if args.action == "summary":
        print(json.dumps(rt.Journal(args.out).summary(), indent=2))
        return 0
    if not args.manifest:
        raise ValueError("--manifest required")
    run = RecoveryRun(rt.load_json(args.manifest), args.out)
    args.out.mkdir(parents=True, exist_ok=True)
    if args.action == "prepared":
        run.prepared(rt.load_json(args.history))
    elif args.action == "nested":
        if not args.message_id:
            raise ValueError("nested requires actual fresh X-to-Y --message-id")
        run.nested(args.message_id)
    elif args.action == "fences":
        run.fences(rt.load_json(args.captured))
    else:
        getattr(run, {"armed": "arm", "repair-observed": "repair_observed"}.get(args.action, args.action))()
    print(json.dumps(run.journal.summary(), indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        # Error type is safe; arbitrary dependency strings can contain private
        # URLs/credentials. Details remain in the private coordinating session.
        print(json.dumps({"failed": True, "error_type": type(error).__name__, "release": False}), file=sys.stderr)
        raise SystemExit(1)
