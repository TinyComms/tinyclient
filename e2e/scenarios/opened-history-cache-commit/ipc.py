"""Minimal chatd IPC helper for fixture setup and diagnostic guards only."""

from __future__ import annotations

import json
import os
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


def request(profile: str, payload: Mapping[str, Any], app_dir: str = "", timeout: int = 60) -> dict[str, Any]:
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
) -> dict[str, Any]:
    return request(
        profile,
        {
            "op": "messages",
            "conversation": {"network": network, "account": account, "id": conversation},
        },
        app_dir=app_dir,
    )


def text_contains_marker(answer: Mapping[str, Any], marker: str) -> bool:
    if answer.get("op") != "messages":
        return False
    return any(
        isinstance(row, Mapping)
        and row.get("text") == marker
        and row.get("seal") == "encrypted"
        and row.get("state", "sent") == "sent"
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
    if target.get("seal") == "undecryptable" and target.get("state", "sent") == "sent":
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
