"""Session environment helper for scratch Tiny runs.

Keeps a literal space in TINYCLIENT_SESSION_TINY SERVER and strips other
TINYCLIENT_SESSION_* keys so a lab host's signed-in profile cannot leak in.
"""

from __future__ import annotations

import json
from typing import Mapping, MutableMapping

SESSION_VARIABLE = "TINYCLIENT_SESSION_TINY SERVER"


def environment(parent: Mapping[str, str], profile: str, session: Mapping[str, object]) -> dict[str, str]:
    result = {
        key: value
        for key, value in parent.items()
        if not key.upper().startswith("TINYCLIENT_SESSION_")
    }
    result["TINYCLIENT_DIR"] = str(profile)
    result["TINYCLIENT_LOG_LEVEL"] = parent.get("TINYCLIENT_LOG_LEVEL", "info")
    result[SESSION_VARIABLE] = json.dumps(session, separators=(",", ":"))
    return result


def connected(status: Mapping[str, object], user_id: str) -> bool:
    if status.get("op") != "status":
        return False
    accounts = status.get("accounts") or []
    if not isinstance(accounts, list):
        return False
    for row in accounts:
        if not isinstance(row, Mapping):
            continue
        key = row.get("key")
        if key == {"network": "tiny", "account": user_id} and row.get("link") == "connected":
            return True
    return False


def apply(env: MutableMapping[str, str], profile: str, session: Mapping[str, object]) -> None:
    built = environment(env, profile, session)
    env.clear()
    env.update(built)
