#!/usr/bin/env python3
"""A call joins, and stays up, on a machine with no microphone.

Starts this build's mediad the way chatd does (frames on stdin and stdout) and joins it to a
voice server this driver runs on localhost: the version 8 voice websocket and its UDP address
discovery. The microphone is withheld with TINYCLIENT_MEDIAD_AUDIO=unplugged, the test stand-in
for a machine that lists none; the speaker is a file, as in every lab run.

Passes only when the call connects, says that nobody can hear this person and names no
microphone, sends no audio, and is still up and answering six seconds later.

What it does not prove: a real device being pulled out of a real machine. The lab cannot take a
host's microphone away, so the stand-in does. A build from before the fix does not know the
stand-in, hears silence in its place and never says the words, so this driver fails there on
the notice step; on a real machine with no microphone that build ended the join instead.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR.
"""

import base64
import hashlib
import json
import os
import queue
import socket
import struct
import subprocess
import sys
import threading
import time

OUT = os.environ.get("E2E_OUT", "")
T0 = float(os.environ.get("E2E_T0") or time.time())
APP = os.environ.get("E2E_APP_DIR", "")

PATIENCE = 20.0
STAYS_UP = 6.0

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


def mediad_bin(app_dir):
    name = "mediad.exe" if sys.platform == "win32" else "mediad"
    if sys.platform == "darwin" and app_dir.endswith(".app"):
        return os.path.join(app_dir, "Contents", "MacOS", "mediad")
    return os.path.join(app_dir, name)


class Mediad:
    """mediad as chatd runs it: a length and a JSON frame each way."""

    def __init__(self, binary, env, log):
        kwargs = {"env": env, "stdin": subprocess.PIPE, "stdout": subprocess.PIPE, "stderr": log}
        if sys.platform == "win32":
            kwargs["creationflags"] = 0x08000000
        self.proc = subprocess.Popen([binary, "1"], **kwargs)
        self.frames = queue.Queue()
        self.seen = []
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        out = self.proc.stdout
        while True:
            head = out.read(4)
            if len(head) < 4:
                return
            body = out.read(struct.unpack(">I", head)[0])
            try:
                self.frames.put(json.loads(body.decode("utf-8")))
            except ValueError:
                return

    def ask(self, request):
        body = json.dumps(request).encode("utf-8")
        try:
            self.proc.stdin.write(struct.pack(">I", len(body)) + body)
            self.proc.stdin.flush()
        except OSError:
            return False
        return True

    def take(self):
        """Everything mediad has said so far."""
        while True:
            try:
                self.seen.append(self.frames.get_nowait())
            except queue.Empty:
                return self.seen

    def said(self, kind):
        return [frame for frame in self.take() if frame.get("media") == kind]

    def wait(self, kind, within=PATIENCE, serve=None):
        deadline = time.time() + within
        while time.time() < deadline:
            found = self.said(kind)
            if found:
                return found[0]
            if self.said("failed") and kind != "failed":
                return None
            if serve:
                serve()
            else:
                time.sleep(0.02)
        return None

    def hang_up(self):
        """Closing stdin is the hang-up. The exit code, or None when it did not go."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            return self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            return None

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=5)


class Voice:
    """A voice server on localhost: the websocket handshake a call needs, and its UDP port."""

    def __init__(self):
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(1)
        self.listener.settimeout(PATIENCE)
        self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp.bind(("127.0.0.1", 0))
        self.udp.settimeout(PATIENCE)
        self.ws = None
        self.held = b""
        self.seq = 0
        # Audio packets from mediad, after address discovery.
        self.rtp = 0

    def endpoint(self):
        return "127.0.0.1:%d" % self.listener.getsockname()[1]

    def accept(self):
        self.ws, _ = self.listener.accept()
        self.ws.settimeout(5)
        request = b""
        while b"\r\n\r\n" not in request:
            piece = self.ws.recv(4096)
            if not piece:
                raise OSError("mediad closed the socket before the upgrade")
            request += piece
        head, self.held = request.split(b"\r\n\r\n", 1)
        key = ""
        for line in head.decode("latin-1").split("\r\n")[1:]:
            name, _, value = line.partition(":")
            if name.strip().lower() == "sec-websocket-key":
                key = value.strip()
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest())
        answer = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n"
        self.ws.sendall(answer)
        self.ws.settimeout(0.02)

    def frame(self, opcode, payload):
        head = bytes([0x80 | opcode])
        if len(payload) < 126:
            head += bytes([len(payload)])
        else:
            head += bytes([126]) + struct.pack(">H", len(payload))
        self.ws.sendall(head + payload)

    def send(self, op, d):
        self.seq += 1
        self.frame(1, json.dumps({"op": op, "d": d, "seq": self.seq}).encode("utf-8"))

    def parse(self):
        """One whole websocket frame off what is held, or None when more bytes are needed."""
        held = self.held
        if len(held) < 2:
            return None
        opcode, length, at = held[0] & 0x0F, held[1] & 0x7F, 2
        masked = held[1] & 0x80
        if length == 126:
            if len(held) < 4:
                return None
            length, at = struct.unpack(">H", held[2:4])[0], 4
        elif length == 127:
            if len(held) < 10:
                return None
            length, at = struct.unpack(">Q", held[2:10])[0], 10
        mask = b""
        if masked:
            if len(held) < at + 4:
                return None
            mask, at = held[at:at + 4], at + 4
        if len(held) < at + length:
            return None
        payload = held[at:at + length]
        self.held = held[at + length:]
        if mask:
            payload = bytes(byte ^ mask[n % 4] for n, byte in enumerate(payload))
        return opcode, payload

    def pump(self):
        """Reads what mediad sent. Heartbeats are answered. Gives the voice frames read."""
        frames = []
        try:
            piece = self.ws.recv(65536)
            if not piece:
                raise OSError("mediad closed the voice socket")
            self.held += piece
        except socket.timeout:
            pass
        while True:
            parsed = self.parse()
            if parsed is None:
                break
            opcode, payload = parsed
            if opcode == 9:
                self.frame(10, payload)
            elif opcode == 8:
                raise OSError("mediad closed the voice socket")
            elif opcode == 1:
                said = json.loads(payload.decode("utf-8"))
                if said.get("op") == 3:
                    self.frame(1, json.dumps({"op": 6, "d": {"t": said["d"]["t"]}}).encode("utf-8"))
                else:
                    frames.append(said)
        self.drain_udp()
        return frames

    def drain_udp(self):
        self.udp.settimeout(0)
        while True:
            try:
                packet, source = self.udp.recvfrom(2048)
            except (BlockingIOError, socket.timeout):
                break
            except OSError:
                break
            if len(packet) == 8:
                # The keepalive, echoed as the voice server does.
                self.udp.sendto(packet, source)
            elif len(packet) > 12 and packet[0] & 0xC0 == 0x80:
                self.rtp += 1

    def expect(self, op):
        deadline = time.time() + PATIENCE
        while time.time() < deadline:
            for said in self.pump():
                if said.get("op") == op:
                    return said.get("d")
        raise OSError("no opcode %d from mediad" % op)

    def admit(self):
        """Identify, hello and ready, address discovery, then the session's key."""
        self.expect(0)
        self.send(8, {"v": 8, "heartbeat_interval": 2000})
        port = self.udp.getsockname()[1]
        modes = ["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"]
        self.send(2, {"ssrc": 1111, "ip": "127.0.0.1", "port": port, "modes": modes})
        self.udp.settimeout(PATIENCE)
        request, source = self.udp.recvfrom(128)
        if len(request) != 74 or request[:4] != bytes([0, 1, 0, 70]):
            raise OSError("address discovery was %d bytes" % len(request))
        answer = bytearray(74)
        answer[:4] = bytes([0, 2, 0, 70])
        answer[8:17] = b"127.0.0.1"
        answer[72:] = struct.pack(">H", source[1])
        self.udp.sendto(bytes(answer), source)
        select = self.expect(1)
        mode = select["data"]["mode"]
        self.send(4, {"mode": mode, "secret_key": [9] * 32, "dave_protocol_version": 0})

    def close(self):
        for sock in (self.ws, self.listener, self.udp):
            if sock is not None:
                try:
                    sock.close()
                except OSError:
                    pass


def run(binary):
    data = os.path.join(OUT, "data")
    os.makedirs(data, exist_ok=True)
    env = os.environ.copy()
    env["TINYCLIENT_DIR"] = data
    env["TINYCLIENT_MEDIAD_AUDIO"] = "unplugged"
    env["TINYCLIENT_MEDIAD_SINK"] = os.path.join(OUT, "played.pcm")
    env["TINYCLIENT_VOICE_PLAIN"] = "1"
    env["TINYCLIENT_LOG_LEVEL"] = "info"
    voice = Voice()
    with open(os.path.join(OUT, "app.err.log"), "w", encoding="utf-8") as log:
        mediad = Mediad(binary, env, log)
        try:
            step("mediad starts", mediad.wait("ready") is not None)
            join = {
                "media": "join",
                "network": "discord",
                "endpoint": voice.endpoint(),
                "token": "voice-token",
                "session_id": "session-1",
                "user_id": "111",
                "server_id": "900",
                "channel_id": "900",
                "dave": {"max_version": 0, "required": False},
                "muted": False,
                "deafened": False,
            }
            asked = mediad.ask(join) and mediad.wait("done") is not None
            step("a join to a localhost voice server is taken", asked)
            try:
                voice.accept()
                voice.admit()
                admitted = True
            except (OSError, KeyError, ValueError) as err:
                print("voice server: %s" % err, flush=True)
                admitted = False
            step("the voice server admits the call", admitted)
            if not admitted:
                return

            connected = mediad.wait("connected", serve=voice.pump)
            failed = mediad.said("failed")
            if failed:
                print("mediad failed: %s" % failed[0].get("detail"), flush=True)
            step("the call connects with no microphone", connected is not None and not failed)

            devices = mediad.wait("devices", within=5, serve=voice.pump) or {}
            notice = devices.get("notice") or ""
            print("devices: %s" % json.dumps(devices), flush=True)
            step("the call says that nobody can hear you", "nobody can hear you" in notice and bool(devices.get("stuck")))
            step("no device is named as the microphone", devices.get("media") == "devices" and devices.get("input") == "")

            until = time.time() + STAYS_UP
            alive = True
            while time.time() < until and alive:
                try:
                    voice.pump()
                except OSError as err:
                    print("voice server: %s" % err, flush=True)
                    alive = False
                alive = alive and mediad.proc.poll() is None
            ended = mediad.said("failed") + mediad.said("interrupted")
            step("the call is still up six seconds later", alive and not ended)
            before = len(mediad.said("done"))
            answered = mediad.ask({"media": "audio", "muted": False, "deafened": False})
            deadline = time.time() + 5
            while answered and len(mediad.said("done")) == before and time.time() < deadline:
                voice.pump()
            step("it still answers", answered and len(mediad.said("done")) > before)
            speaking = [frame for frame in mediad.said("speaking") if frame.get("user_id") == "111"]
            step("nothing was sent as this person's voice", voice.rtp == 0 and not speaking)
            step("hanging up ends it cleanly", mediad.hang_up() == 0)
        finally:
            mediad.kill()
            voice.close()


def main():
    global STEPS
    if not OUT or not APP:
        print("E2E_OUT and E2E_APP_DIR are required", file=sys.stderr)
        return 1
    os.makedirs(OUT, exist_ok=True)
    STEPS = os.path.join(OUT, "steps.jsonl")
    open(STEPS, "w", encoding="utf-8").close()
    binary = mediad_bin(APP)
    if not os.path.isfile(binary):
        step("mediad is in E2E_APP_DIR", False)
        return 1
    try:
        run(binary)
    except Exception as err:  # A driver that crashes is a failed step, not a pass.
        step("the driver ran to its end (%s)" % type(err).__name__, False)
        print(repr(err), flush=True)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
