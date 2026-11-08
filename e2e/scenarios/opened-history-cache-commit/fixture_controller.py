"""Owned synthetic fixture controller for the sealed-cache append fault.

This module prepares and mutates disposable receiver/sender profiles. It does not
drive the person-facing window. Diagnostic IPC is allowed here; native pixel
proof belongs in native_windows.py / drive.windows.ps1.

Fault class (cache-write-order199): an empty directory occupies the otherwise
absent sealed-cache log path `e2ee/Tiny-<user>/text/cache-<conversation>.log`
while its parent remains writable. Only that append fails.

Environment (all paths absolute; no host/provider names or secrets in-tree):
  E2E_CACHE_COMMIT_FIXTURE  JSON file describing the run (required for a matched case)
"""

from __future__ import annotations

import json
import base64
import binascii
import os
import sqlite3
import time
from pathlib import Path
from typing import Any, Mapping


MARKER_PREFIX = "opened-history-cache-commit-"


class FixtureError(RuntimeError):
    """Setup failed for a reason that is not the history-loss assertion."""


def load_contract(path: str | None = None) -> dict[str, Any]:
    raw = path or os.environ.get("E2E_CACHE_COMMIT_FIXTURE", "").strip()
    if not raw:
        raise FixtureError(
            "E2E_CACHE_COMMIT_FIXTURE must name an owned synthetic fixture JSON; "
            "missing fixture is not a valid base RED"
        )
    contract_path = Path(raw)
    if not contract_path.is_file():
        raise FixtureError("fixture contract file is missing: %s" % contract_path)
    data = json.loads(contract_path.read_text(encoding="utf-8"))
    for key in ("marker", "conversation_id", "receiver", "sender"):
        if key not in data:
            raise FixtureError("fixture contract missing %s" % key)
    if not str(data["marker"]).startswith(MARKER_PREFIX):
        raise FixtureError("fixture marker must start with %s" % MARKER_PREFIX)
    # Scratch sessions bypass the keychain. Never let each restart silently create
    # new wrapping keys and discard the device state this scenario is testing.
    for role in ("receiver", "sender"):
        person = data[role]
        session = person.get("session", {})
        keys = session.get("e2ee") if isinstance(session, Mapping) else None
        if not isinstance(keys, Mapping) or session.get("user_id") != person.get("user_id"):
            raise FixtureError("%s needs its own stable session.e2ee wrapping keys before first launch" % role)
        name = session.get("display_name")
        if not isinstance(name, str) or not name or name.split() != [name]:
            raise FixtureError("%s needs a single-word synthetic display_name for sidebar pixel selection" % role)
        for name in ("state", "cache"):
            try:
                encoded = keys.get(name)
                if not isinstance(encoded, str) or len(base64.b64decode(encoded, validate=True)) != 32:
                    raise ValueError("invalid key")
            except (ValueError, binascii.Error):
                raise FixtureError("%s has missing/invalid durable wrapping keys" % role) from None
    return data


def receiver_profile(contract: Mapping[str, Any]) -> Path:
    profile = Path(contract["receiver"]["profile"])
    if not profile.is_absolute():
        raise FixtureError("receiver.profile must be absolute")
    return profile


def sender_profile(contract: Mapping[str, Any]) -> Path:
    profile = Path(contract["sender"]["profile"])
    if not profile.is_absolute():
        raise FixtureError("sender.profile must be absolute")
    return profile


def e2ee_text_dir(profile: Path, user_id: str) -> Path:
    safe = "".join(
        ch if ch.isalnum() or ch == "-" else "_" for ch in ("Tiny-%s" % user_id)
    )[:120]
    return profile / "e2ee" / safe / "text"


def cache_log_path(profile: Path, user_id: str, conversation_id: str) -> Path:
    return e2ee_text_dir(profile, user_id) / ("cache-%s.log" % conversation_id)


def place_append_blocker(contract: Mapping[str, Any]) -> Path:
    """Create an empty directory at the sealed-cache log path. Parent must stay writable."""
    receiver = contract["receiver"]
    profile = receiver_profile(contract)
    text_dir = e2ee_text_dir(profile, receiver["user_id"])
    text_dir.mkdir(parents=True, exist_ok=True)
    path = cache_log_path(profile, receiver["user_id"], contract["conversation_id"])
    if path.is_file():
        raise FixtureError("cache log already exists; refusing to overwrite prior sealed history")
    if path.is_dir():
        return path
    if path.exists():
        raise FixtureError("cache path exists with unexpected type: %s" % path)
    path.mkdir(parents=False, exist_ok=False)
    if not path.is_dir():
        raise FixtureError("failed to place empty-directory cache blocker")
    # Parent must remain a normal directory (writable state store lives beside the cache).
    if not text_dir.is_dir():
        raise FixtureError("e2ee text parent disappeared after blocker placement")
    return path


def clear_append_blocker(contract: Mapping[str, Any]) -> None:
    path = cache_log_path(
        receiver_profile(contract),
        contract["receiver"]["user_id"],
        contract["conversation_id"],
    )
    if path.is_dir():
        path.rmdir()
    elif path.exists() and not path.is_file():
        raise FixtureError("cannot clear blocker at unexpected path type: %s" % path)


def wipe_message_index(contract: Mapping[str, Any]) -> dict[str, Any]:
    """Delete only message rows for this fixture conversation; keep conversation/MLS/keys."""
    profile = receiver_profile(contract)
    db_path = profile / "chat.sqlite"
    if not db_path.is_file():
        raise FixtureError("receiver chat.sqlite missing")
    conv = contract["conversation_id"]
    scope = ("tiny", contract["receiver"]["user_id"], conv)
    uri = "file:%s?mode=rw" % db_path.as_posix()
    conn = sqlite3.connect(uri, uri=True, timeout=30)
    conn.isolation_level = None
    try:
        conn.execute("PRAGMA busy_timeout=30000")
        conn.execute("BEGIN IMMEDIATE")
        conv_rows = conn.execute(
            "SELECT COUNT(*) FROM conversation WHERE network = ? AND account = ? AND id = ?",
            scope,
        ).fetchone()[0]
        if conv_rows != 1:
            raise FixtureError("receiver conversation missing; refusing to change message index")
        before = conn.execute(
            "SELECT COUNT(*) FROM message WHERE network = ? AND account = ? AND conversation = ?", scope
        ).fetchone()[0]
        deleted = conn.execute(
            "DELETE FROM message WHERE network = ? AND account = ? AND conversation = ?", scope
        ).rowcount
        conn.execute("COMMIT")
        after = conn.execute(
            "SELECT COUNT(*) FROM message WHERE network = ? AND account = ? AND conversation = ?", scope
        ).fetchone()[0]
    finally:
        conn.close()
    if conv_rows < 1:
        raise FixtureError("conversation row missing after message wipe; wrong fixture")
    if after != 0:
        raise FixtureError("message index wipe left rows for the target conversation")
    return {"before": before, "deleted": deleted, "after": after, "conversation_rows": conv_rows}


def cache_is_file(contract: Mapping[str, Any]) -> bool:
    path = cache_log_path(
        receiver_profile(contract),
        contract["receiver"]["user_id"],
        contract["conversation_id"],
    )
    return path.is_file()


def cache_blocker_present(contract: Mapping[str, Any]) -> bool:
    path = cache_log_path(
        receiver_profile(contract),
        contract["receiver"]["user_id"],
        contract["conversation_id"],
    )
    return path.is_dir()


def write_receipt(out_dir: str, name: str, payload: Mapping[str, Any]) -> Path:
    path = Path(out_dir) / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def wait_until(seconds: float, predicate, sleep: float = 0.2) -> bool:
    deadline = time.time() + seconds
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(sleep)
    return False
