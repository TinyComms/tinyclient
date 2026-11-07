#!/usr/bin/env python3
"""Normal-app sealed-cache append fault: base loses history; branch keeps it.

This driver does NOT use TINYCLIENT_PROVE. Fixture setup lives in
fixture_controller.py; person-facing observation uses native_windows.py.

Matched case (same protocol on base and branch):
  1. Owned synthetic receiver/sender profiles from E2E_CACHE_COMMIT_FIXTURE
  2. Empty-directory blocker on the absent sealed-cache log path
  3. Owned peer sends a unique encrypted marker
  4. Observe under fault (base must decrypt; fixed must refuse durable open)
  5. Cold stop; remove blocker; wipe only this fixture's SQLite message index
  6. Restart and assert the shared step:
     "opened plaintext is still present after cache-fault repair and cold restart"

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR, E2E_CACHE_COMMIT_FIXTURE.
E2E_SABOTAGE=1 forces a failing observation without claiming history-loss RED.
Wrong session/login/group or missing fixture is setup-incomplete, not valid RED.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

import fixture_controller as fixture
import ipc
import session_contract

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")
SABOTAGE = os.environ.get("E2E_SABOTAGE") == "1"

FAILED = False
STEPS = ""
STARTED: list[subprocess.Popen] = []


def now() -> float:
    return time.time() - T0


def step(words: str, ok: bool, shot: str = "") -> None:
    global FAILED
    if not ok:
        FAILED = True
    payload = {"t": round(now(), 1), "step": words, "ok": bool(ok)}
    if shot:
        payload["shot"] = shot
    line = json.dumps(payload, ensure_ascii=True)
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def binaries(app_dir: str):
    if sys.platform == "win32":
        return (
            os.path.join(app_dir, "chatd.exe"),
            os.path.join(app_dir, "tinyclient.exe"),
            os.path.join(app_dir, "mediad.exe"),
        )
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        mac = os.path.join(app_dir, "Contents", "MacOS")
        return os.path.join(mac, "chatd"), os.path.join(mac, "tinyclient"), os.path.join(mac, "mediad")
    return os.path.join(app_dir, "chatd"), os.path.join(app_dir, "tinyclient"), os.path.join(app_dir, "mediad")


def popen(args, env, cwd, out_log, err_log, window=False):
    kwargs = {
        "cwd": cwd,
        "env": env,
        "stdin": subprocess.DEVNULL,
        "stdout": open(out_log, "w", encoding="utf-8"),
        "stderr": open(err_log, "w", encoding="utf-8"),
    }
    if sys.platform == "win32":
        kwargs["creationflags"] = 0 if window else 0x08000000
    proc = subprocess.Popen(args, **kwargs)
    STARTED.append(proc)
    return proc


def stop_owned() -> None:
    for proc in reversed(STARTED):
        if proc.poll() is not None:
            continue
        try:
            proc.terminate()
            proc.wait(timeout=8)
        except Exception:
            try:
                proc.kill()
                proc.wait(timeout=8)
            except Exception:
                raise fixture.FixtureError("owned process did not stop; refusing cold-restart proof")


def wait_connected(profile: str, user_id: str, app_dir: str, seconds: float = 60.0) -> bool:
    def ready():
        try:
            answer = ipc.status(profile, app_dir)
        except Exception:
            return False
        return session_contract.connected(answer, user_id)

    return fixture.wait_until(seconds, ready)


def wait_target(profile: str, app_dir: str, contract: dict, message_id: str, seconds=45):
    key = {"network": "tiny", "account": contract["receiver"]["user_id"],
           "id": contract["conversation_id"]}
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        answer = ipc.messages(profile, app_dir, "tiny", key["account"], key["id"])
        state = ipc.target_state(answer, key, message_id, contract["marker"])
        if state != "missing":
            return state, answer
        time.sleep(0.5)  # Messages answers the cache while queuing asynchronous backfill.
    raise fixture.FixtureError("exact delivered message never appeared; no history-loss RED")


def wait_delivered(profile, app_dir, contract, seconds=60):
    key = {"network": "tiny", "account": contract["sender"]["user_id"],
           "id": contract["conversation_id"]}
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        answer = ipc.messages(profile, app_dir, "tiny", key["account"], key["id"])
        message_id = ipc.delivered_id(answer, key, contract["marker"])
        if message_id:
            return message_id
        time.sleep(0.5)
    raise fixture.FixtureError("peer send never acquired an encrypted server message id")


def incomplete(reason: str, **extra) -> int:
    step("owned synthetic fixture is ready (%s)" % reason, False)
    payload = {"error": reason, "valid_red": False}
    payload.update(extra)
    fixture.write_receipt(OUT, "setup-incomplete.json", payload)
    return 1


def classify_case(during_state, after_state, during_pixels, after_pixels):
    if during_state == "opened" and after_state == "undecryptable" and during_pixels is True and after_pixels is False:
        return "base-history-loss-red"
    if during_state == "undecryptable" and after_state == "opened" and during_pixels is False and after_pixels is True:
        return "branch-recoverable-green"
    return "setup-incomplete"


def main() -> int:
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()

    if os.environ.get("TINYCLIENT_PROVE"):
        step("TINYCLIENT_PROVE must not be set for this normal-app driver", False)
        return 1

    chatd_bin, tiny_bin, mediad_bin = binaries(APP)
    if not os.path.isfile(chatd_bin) or not os.path.isfile(tiny_bin):
        step("release-profile chatd and tinyclient are in E2E_APP_DIR", False)
        return 1
    if not os.path.isfile(mediad_bin):
        step("mediad is in E2E_APP_DIR for a coherent bundle", False)
        return 1

    if SABOTAGE:
        step("opened plaintext is still present after cache-fault repair and cold restart", False)
        return 1

    try:
        contract = fixture.load_contract()
    except fixture.FixtureError as err:
        return incomplete(str(err))

    if sys.platform != "win32":
        return incomplete("Windows native pixel driver required", platform=sys.platform)

    import native_windows as native

    native.require_windows()
    receiver = contract["receiver"]
    sender = contract["sender"]
    recv_profile = str(fixture.receiver_profile(contract))
    send_profile = str(fixture.sender_profile(contract))
    marker = contract["marker"]
    conv = contract["conversation_id"]

    shots = Path(OUT) / "shots"
    shots.mkdir(parents=True, exist_ok=True)

    recv_env = session_contract.environment(os.environ, recv_profile, receiver["session"])
    send_env = session_contract.environment(os.environ, send_profile, sender["session"])

    window = None
    try:
        cache_path = fixture.cache_log_path(
            fixture.receiver_profile(contract), receiver["user_id"], conv
        )
        if cache_path.exists() and not cache_path.is_dir():
            return incomplete("sealed-cache path already occupied by a file")

        blocker = fixture.place_append_blocker(contract)
        step("empty-directory blocker occupies the absent sealed-cache log path", blocker.is_dir())
        fixture.write_receipt(OUT, "blocker.json", {"basename": blocker.name, "is_dir": True})

        popen(
            [chatd_bin],
            send_env,
            send_profile,
            os.path.join(OUT, "sender-chatd.out.log"),
            os.path.join(OUT, "sender-chatd.err.log"),
        )
        popen(
            [chatd_bin],
            recv_env,
            recv_profile,
            os.path.join(OUT, "receiver-chatd.out.log"),
            os.path.join(OUT, "receiver-chatd.err.log"),
        )
        send_ok = wait_connected(send_profile, sender["user_id"], APP, 90)
        recv_ok = wait_connected(recv_profile, receiver["user_id"], APP, 90)
        step("owned sender and receiver chatd are connected", send_ok and recv_ok)
        if not (send_ok and recv_ok):
            return incomplete(
                "chatd not connected",
                sender_connected=send_ok,
                receiver_connected=recv_ok,
            )

        def e2ee_ready(profile: str, user_id: str) -> bool:
            try:
                answer = ipc.e2ee_status(profile, APP, user_id)
            except Exception:
                return False
            return ipc.e2ee_device_state(answer) == "ready"

        send_e2ee = fixture.wait_until(90, lambda: e2ee_ready(send_profile, sender["user_id"]))
        recv_e2ee = fixture.wait_until(90, lambda: e2ee_ready(recv_profile, receiver["user_id"]))
        step("owned sender and receiver e2ee devices are ready", send_e2ee and recv_e2ee)
        if not (send_e2ee and recv_e2ee):
            return incomplete(
                "e2ee device not ready",
                sender_e2ee=send_e2ee,
                receiver_e2ee=recv_e2ee,
            )

        client_id = "cache-commit-%s" % uuid.uuid4().hex[:12]
        try:
            sent = ipc.send_text(
                send_profile, APP, "tiny", sender["user_id"], conv, marker, client_id
            )
            send_ok = sent.get("op") != "error"
        except Exception as err:
            send_ok = False
            sent = {"error": str(err)}
        step("owned peer delivered the unique encrypted marker", send_ok)
        fixture.write_receipt(OUT, "peer-send.json", {"ok": send_ok, "client_id": client_id, "answer": sent})
        if not send_ok:
            return incomplete("peer send failed", answer=sent)

        message_id = wait_delivered(send_profile, APP, contract)
        fixture.write_receipt(OUT, "delivered-message.json", {"message_id": message_id})
        during_state, during_ipc = wait_target(recv_profile, APP, contract, message_id)
        during_plain = during_state == "opened"
        fixture.write_receipt(OUT, "during-fault-ipc.json", {
            "plain": during_plain,
            "answer": during_ipc,
        })

        window = popen(
            [tiny_bin],
            recv_env,
            recv_profile,
            os.path.join(OUT, "receiver-gui.out.log"),
            os.path.join(OUT, "receiver-gui.err.log"),
            window=True,
        )
        shown = native.wait_window(window.pid, 60)
        step("receiver window is visible under the fault", shown)
        if not shown:
            return incomplete("receiver window never appeared")
        step("open the peer through Jump under the fault", True)
        peer_name = sender["session"]["display_name"]
        selected = native.select_sidebar_peer(window.pid, peer_name, str(shots / "before-select-during.png"))
        fixture.write_receipt(OUT, "during-navigation.json", selected)
        time.sleep(2.0)
        during_shot = str(shots / "during-fault.png")
        native.shot(window.pid, during_shot, printwindow=False)
        during_pixels = native.conversation_mentions_marker(during_shot, marker)
        step("during-fault window captured for pixel review", True, "shots/during-fault.png")
        if during_pixels is None:
            return incomplete("pixel text oracle unavailable; no native proof claimed")

        if window.poll() is None:
            window.terminate()
            try:
                window.wait(timeout=15)
            except Exception:
                window.kill()
        window = None
        stop_owned()
        STARTED.clear()
        time.sleep(1.0)
        step("owned apps and daemons stopped cold", True)

        fixture.clear_append_blocker(contract)
        wiped = fixture.wipe_message_index(contract)
        wipe_ok = wiped["after"] == 0 and wiped["conversation_rows"] >= 1
        step("blocker removed and only this fixture's SQLite message index emptied", wipe_ok)
        fixture.write_receipt(OUT, "wipe.json", wiped)
        if not wipe_ok:
            return incomplete("sqlite wipe failed", wipe=wiped)

        popen(
            [chatd_bin],
            recv_env,
            recv_profile,
            os.path.join(OUT, "receiver-chatd-restart.out.log"),
            os.path.join(OUT, "receiver-chatd-restart.err.log"),
        )
        recv_ok = wait_connected(recv_profile, receiver["user_id"], APP, 90)
        step("receiver chatd reconnected after repair", recv_ok)
        if not recv_ok:
            return incomplete("receiver did not reconnect after repair")
        after_state, after_ipc = wait_target(recv_profile, APP, contract, message_id)
        after_plain = after_state == "opened"
        cache_file = fixture.cache_is_file(contract)
        fixture.write_receipt(OUT, "after-restart-ipc.json", {
            "plain": after_plain,
            "cache_is_file": cache_file,
            "during_plain": during_plain,
            "answer": after_ipc,
        })

        window = popen(
            [tiny_bin],
            recv_env,
            recv_profile,
            os.path.join(OUT, "receiver-gui-restart.out.log"),
            os.path.join(OUT, "receiver-gui-restart.err.log"),
            window=True,
        )
        shown = native.wait_window(window.pid, 60)
        step("receiver window is visible after restart", shown)
        if not shown:
            return incomplete("receiver window never appeared after restart")
        step("open the peer through Jump after restart", True)
        selected = native.select_sidebar_peer(window.pid, peer_name, str(shots / "before-select-after.png"))
        fixture.write_receipt(OUT, "after-navigation.json", selected)
        time.sleep(2.0)
        after_shot = str(shots / "after-restart.png")
        native.shot(window.pid, after_shot, printwindow=False)
        after_pixels = native.conversation_mentions_marker(after_shot, marker)
        step("after-restart window captured for pixel review", True, "shots/after-restart.png")
        if after_pixels is None:
            return incomplete("pixel text oracle unavailable after restart")

        # Classify the matched case. Wrong login / never decrypted / generic loss is incomplete.
        case = classify_case(during_state, after_state, during_pixels, after_pixels)
        fixture.write_receipt(OUT, "pixel-observations.json", {
            "during": during_pixels, "after": after_pixels, "class": case,
            "during_state": during_state, "after_state": after_state, "message_id": message_id,
        })
        if case == "base-history-loss-red":
            step("message opened while the sealed-cache append was blocked", True)
            step(
                "opened plaintext is still present after cache-fault repair and cold restart",
                False,
            )
            fixture.write_receipt(OUT, "proof-local.json", {
                "class": "base-history-loss-red",
                "valid_red": True,
                "during_plain": True,
                "after_plain": False,
                "cache_is_file": cache_file,
            })
        elif case == "branch-recoverable-green":
            if not cache_file:
                return incomplete("opened row has no durable cache file after repair")
            step("failed cache commit did not claim a durable open", True)
            step(
                "opened plaintext is still present after cache-fault repair and cold restart",
                True,
            )
            fixture.write_receipt(OUT, "proof-local.json", {
                "class": "branch-recoverable-green",
                "valid_red": False,
                "during_plain": False,
                "after_plain": True,
                "cache_is_file": cache_file,
            })
        elif during_plain and after_plain:
            return incomplete(
                "plaintext remained after wipe/restart; not the sealed-cache history-loss case",
                during_plain=True,
                after_plain=True,
                cache_is_file=cache_file,
            )
        else:
            return incomplete(
                "message states and window pixels do not demonstrate the matched history transition",
                during_state=during_state,
                after_state=after_state,
                during_pixels=during_pixels,
                after_pixels=after_pixels,
            )

        return 1 if FAILED else 0
    except fixture.FixtureError as err:
        return incomplete(str(err))
    except Exception as err:
        step("driver: %s" % err, False)
        fixture.write_receipt(OUT, "driver-error.json", {"error": str(err), "valid_red": False})
        return 1
    finally:
        if window and window.poll() is None:
            try:
                window.terminate()
                window.wait(timeout=10)
            except Exception:
                pass
        stop_owned()


if __name__ == "__main__":
    sys.exit(main())
