"""Minimal chatd IPC helper for fixture setup and diagnostic guards only."""

from __future__ import annotations

import json
import os
import subprocess
import sys
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
    return marker in json.dumps(answer)
