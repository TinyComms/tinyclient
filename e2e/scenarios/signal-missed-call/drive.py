#!/usr/bin/env python3
"""A Signal call rings as a notification, and one nobody answered leaves a missed-call line.

Starts this build's chatd with the scripted Signal wire (TINYCLIENT_SIGNAL_FAKE), steers it
over its control file the way tests/signal.rs does, and speaks chatd's real socket: a call
offer records Grace's row, its hangup leaves one grey "Missed voice call" line unread by one,
a video offer says "Missed video call", and a call answered on the phone leaves no line. A
build from before the fix records no row at all, so this driver fails there.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR.
"""

import json
import os
import socket
import struct
import subprocess
import sys
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")

ME = "a1a1a1a1-0000-4000-8000-000000000001"
GRACE = "b2b2b2b2-0000-4000-8000-000000000002"
ALAN = "c3c3c3c3-0000-4000-8000-000000000003"
SESSION = json.dumps({"aci": ME, "name": "Ada Quill", "vault": "00"})
T0MS = 1_791_892_800_000

FAILED = False
STEPS = ""


def now():
    return time.time() - T0


def step(words, ok):
    global FAILED
    if not ok:
        FAILED = True
    line = '{"t": %s, "step": "%s", "ok": %s}' % (round(now(), 1), words.replace('"', "'"), "true" if ok else "false")
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    if STEPS:
        with open(STEPS, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def app_bin(app_dir):
    name = "chatd.exe" if sys.platform == "win32" else "chatd"
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "chatd")
    return os.path.join(app_dir, name)


def connect(sock_path):
    """chatd's socket: a file on Unix, the matching named pipe on Windows."""
    if sys.platform == "win32":
        return open("\\\\.\\pipe\\" + sock_path, "r+b", buffering=0)
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(5)
    stream.connect(sock_path)
    return stream


def write_all(stream, data):
    view = memoryview(data)
    while view:
        written = stream.write(view)
        if not written:
            raise RuntimeError("chatd's socket took nothing")
        view = view[written:]


class Chat:
    def __init__(self, sock_path):
        self.stream = connect(sock_path)

    def ask(self, request):
        body = json.dumps(request, separators=(",", ":")).encode()
        write_all(self.stream, struct.pack("!I", len(body)) + body)
        head = b""
        while len(head) < 4:
            got = self.stream.read(4 - len(head))
            if not got:
                raise RuntimeError("chatd closed its socket")
            head += got
        length = struct.unpack("!I", head)[0]
        if length > 1024 * 1024:
            raise RuntimeError("answer exceeds the protocol size limit")
        chunks = b""
        while len(chunks) < length:
            got = self.stream.read(length - len(chunks))
            if not got:
                raise RuntimeError("chatd closed its socket mid-frame")
            chunks += got
        return json.loads(chunks)

    def close(self):
        try:
            self.stream.close()
        except OSError:
            pass


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    binary = app_bin(APP)
    if not os.path.isfile(binary):
        step("chatd is in E2E_APP_DIR", False)
        return 1

    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    sock_path = os.path.join(data, "chat.sock")
    control = os.path.join(data, "signal.ctl")
    open(control, "a", encoding="utf-8").close()
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_SIGNAL_FAKE"] = "1"
    env["TINYCLIENT_SESSION_SIGNAL"] = SESSION
    env.pop("TINYCLIENT_SESSION_DISCORD", None)
    env.pop("TINYCLIENT_SESSION_SLACK", None)
    env.pop("TINYCLIENT_SESSION_TELEGRAM", None)
    env.pop("TINYCLIENT_SESSION_TEAMS", None)
    kwargs = {"cwd": data, "env": env, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = 0x08000000
    with open(os.path.join(OUT, "chatd.out.log"), "w", encoding="utf-8") as out, open(os.path.join(OUT, "chatd.err.log"), "w", encoding="utf-8") as err:
        kwargs["stdout"] = out
        kwargs["stderr"] = err
        proc = subprocess.Popen([binary], **kwargs)
        chat = None
        try:
            deadline = time.time() + 15
            while chat is None:
                if proc.poll() is not None:
                    step("chatd stays up for the driver", False)
                    return 1
                if time.time() > deadline:
                    step("chatd opens its socket", False)
                    return 1
                try:
                    chat = Chat(sock_path)
                except OSError:
                    time.sleep(0.1)
            drive(chat, control)
        finally:
            if chat:
                chat.close()
            proc.kill()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    return 1 if FAILED else 0


def key(id):
    return {"network": "signal", "account": ME, "id": id}


def wait(what, chat, look, seconds=20):
    deadline = time.time() + seconds
    while time.time() < deadline:
        found = look()
        if found is not None:
            step(what, True)
            return found
        time.sleep(0.2)
    step(what, False)
    return None


def steer(control, line):
    with open(control, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")


def ask(chat, request):
    try:
        return chat.ask(request)
    except Exception:
        return None


def conversation(chat, id):
    answer = ask(chat, {"op": "conversation", "conversation": key(id)})
    return (answer or {}).get("conversation")


def messages(chat, id):
    answer = ask(chat, {"op": "messages", "conversation": key(id), "page": "newest", "limit": 100})
    return (answer or {}).get("messages", [])


def drive(chat, control):
    status = wait("chatd answers its status", chat, lambda: ask(chat, {"op": "status"}))
    if status is None:
        return
    wait("Signal is connected", chat, lambda: (any(a["link"] == "connected" for a in ask(chat, {"op": "status"})["accounts"]) or None))
    group = "g." + "5a175a175a175a175a175a175a175a175a175a175a175a175a175a175a175a17"
    wait("the fake's group is known", chat, lambda: conversation(chat, group) and True)

    # The offer: Grace's row appears with nothing in it.
    at = T0MS + 5_000
    steer(control, "call grace %d" % at)
    row = wait("a call offer records the conversation", chat, lambda: conversation(chat, GRACE))
    if row is not None:
        step("the row is a direct one named after the caller", row.get("kind") == "direct" and row.get("title") == "Grace Ombra")
        step("a ring is not a message", messages(chat, GRACE) == [])

    # The caller gave up: one grey missed-call line, unread by one.
    steer(control, "hangup %d" % at)
    line = wait("the hangup leaves the missed-call line", chat, lambda: next((m for m in messages(chat, GRACE) if m["id"] == "%d-%s" % (at, GRACE)), None))
    if line is not None:
        step("the line is a grey system line", line.get("kind") == {"kind": "system"})
        step("the line says the call was missed", "Missed voice call" == line.get("text"))
        step("the line names the caller", line.get("sender", {}).get("name") == "Grace Ombra")
    wait("the line counts as one unread", chat, lambda: (conversation(chat, GRACE) or {}).get("unread") == 1)

    # A video offer missed reads as one.
    video_at = T0MS + 6_000
    steer(control, "call-video grace %d" % video_at)
    steer(control, "hangup %d" % video_at)
    wait("a missed video call says so", chat, lambda: any(m.get("text") == "Missed video call" for m in messages(chat, GRACE)))

    # Answered on the phone: the ring ends and no line is left.
    alan_at = T0MS + 7_000
    steer(control, "call alan %d" % alan_at)
    wait("Alan's row is recorded too", chat, lambda: conversation(chat, ALAN))
    steer(control, "hangup-accepted %d" % alan_at)
    time.sleep(1.0)
    step("an answered call leaves no line", messages(chat, ALAN) == [])

    # A hangup for a call we never heard of is nothing.
    steer(control, "busy %d" % (T0MS + 8_000))
    time.sleep(1.0)
    step("a hangup with no offer of ours leaves nothing", messages(chat, ALAN) == [])


if __name__ == "__main__":
    sys.exit(main())
