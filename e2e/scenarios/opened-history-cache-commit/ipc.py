"""Minimal chatd IPC helper for fixture setup and diagnostic guards only."""

from __future__ import annotations

import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import uuid
from typing import Any, Mapping


def dlctl_bin(app_dir: str) -> str:
    override = os.environ.get("E2E_DLCTL", "").strip()
    if override and os.path.isfile(override):
        return override
    name = "dlctl.exe" if sys.platform == "win32" else "dlctl"
    path = os.path.join(app_dir, name)
    if os.path.isfile(path):
        return path
    return ""


def _unix_request(profile, payload, timeout):
    directory = Path(profile)
    if not directory.is_absolute():
        raise RuntimeError("fixture profile must be absolute")
    body = json.dumps(payload, separators=(",", ":")).encode()
    if len(body) > 1024 * 1024:  # protocol::FRAME_LIMIT
        raise RuntimeError("fixture IPC request exceeds the protocol size limit")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(timeout)
        stream.connect(str(directory / "chat.sock"))
        stream.sendall(struct.pack("!I", len(body)) + body)

        def read_exact(length):
            chunks = bytearray()
            while len(chunks) < length:
                chunk = stream.recv(length - len(chunks))
                if not chunk:
                    raise RuntimeError("fixture IPC closed before the frame completed")
                chunks.extend(chunk)
            return bytes(chunks)

        length = struct.unpack("!I", read_exact(4))[0]
        if length > 1024 * 1024:
            raise RuntimeError("fixture IPC answer exceeds the diagnostic size limit")
        answer = json.loads(read_exact(length))
        if not isinstance(answer, dict):
            raise RuntimeError("fixture IPC answer is not an object")
        return answer


def request(profile: str, payload: Mapping[str, Any], app_dir: str = "", timeout: int = 60) -> dict[str, Any]:
    if sys.platform.startswith("linux") and not os.environ.get("E2E_DLCTL", "").strip():
        return _unix_request(profile, payload, timeout)
    binary = dlctl_bin(app_dir) if app_dir else dlctl_bin("")
    if not binary:
        raise RuntimeError(
            "dlctl is required for fixture IPC (place dlctl beside the app or set E2E_DLCTL)"
        )
    body = json.dumps(payload, separators=(",", ":"))
    done = subprocess.run(
        [binary, profile, body],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if done.returncode not in (0, 4):
        raise RuntimeError(
            "dlctl failed (%s): %s" % (done.returncode, (done.stderr or done.stdout)[-500:])
        )
    return json.loads(done.stdout or "{}")


def status(profile: str, app_dir: str) -> dict[str, Any]:
    return request(profile, {"op": "status"}, app_dir=app_dir)


def e2ee_status(profile: str, app_dir: str, user_id: str) -> dict[str, Any]:
    return request(
        profile,
        {
            "op": "tiny_e2ee",
            "account": {"network": "tiny", "account": user_id},
            "ask": {"op": "status"},
        },
        app_dir=app_dir,
        timeout=45,
    )


def e2ee_device_state(answer: Mapping[str, Any]) -> str:
    status = answer.get("status") if isinstance(answer.get("status"), Mapping) else answer
    if not isinstance(status, Mapping):
        return ""
    device = status.get("device")
    if isinstance(device, Mapping):
        return str(device.get("state") or "")
    return str(device or "")


def send_text(
    profile: str,
    app_dir: str,
    network: str,
    account: str,
    conversation: str,
    text: str,
    client_id: str,
) -> dict[str, Any]:
    return request(
        profile,
        {
            "op": "send",
            "conversation": {"network": network, "account": account, "id": conversation},
            "client_id": client_id,
            "text": text,
        },
        app_dir=app_dir,
    )


def messages(
    profile: str,
    app_dir: str,
    network: str,
    account: str,
    conversation: str,
    limit: int = 50,
) -> dict[str, Any]:
    return request(
        profile,
        {
            "op": "messages",
            "conversation": {"network": network, "account": account, "id": conversation},
            "limit": int(limit),
        },
        app_dir=app_dir,
    )


def row_send_state(row: Mapping[str, Any]) -> str:
    if "state" not in row:
        return "sent"  # serde omits the default Sent variant.
    state = row["state"]
    if isinstance(state, Mapping):
        value = state.get("state")
        return value if value in ("sent", "pending", "failed") else "invalid"
    return "invalid"


def text_contains_marker(answer: Mapping[str, Any], marker: str) -> bool:
    if answer.get("op") != "messages":
        return False
    return any(
        isinstance(row, Mapping)
        and row.get("text") == marker
        and row.get("seal") == "encrypted"
        and row_send_state(row) == "sent"
        for row in answer.get("messages", [])
    )


def target_state(answer, conversation, message_id, marker):
    if answer.get("op") != "messages" or answer.get("conversation") != conversation:
        raise ValueError("history reply is not a messages page for the expected account/conversation")
    rows = answer.get("messages")
    if not isinstance(rows, list):
        raise ValueError("history reply has no messages array")
    target = next((row for row in rows if isinstance(row, Mapping) and row.get("id") == message_id), None)
    if target is None:
        return "missing"
    if text_contains_marker({"op": "messages", "messages": [target]}, marker):
        return "opened"
    if target.get("seal") == "undecryptable" and row_send_state(target) == "sent":
        return "undecryptable"
    raise ValueError("target row is neither the exact opened message nor an undecryptable row")


def delivered_id(answer, conversation, marker):
    if answer.get("op") != "messages" or answer.get("conversation") != conversation:
        raise ValueError("sender history reply is not scoped to the expected conversation")
    for row in answer.get("messages", []):
        if not isinstance(row, Mapping):
            continue
        if not text_contains_marker({"op": "messages", "messages": [row]}, marker):
            continue
        try:
            return str(uuid.UUID(row.get("id", "")))
        except (ValueError, TypeError, AttributeError):
            continue
    return None
