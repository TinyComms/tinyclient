#!/usr/bin/env python3
"""A Tiny server that posts two direct messages in one second, out of id order.

The later post has the id that sorts first. Both carry the server's sequence. A client that
keeps the sequence shows "first" then "second". One that sorts the id shows them backwards.

Standard library only. `python fixture.py --self-test` checks the trap without chatd.
"""

import base64
import hashlib
import json
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

TOKEN = "fixture-token"
ME = "11111111-1111-4111-8111-111111111111"
BEA = "22222222-2222-4222-8222-222222222222"
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
WHEN = "2024-01-02T03:04:05Z"
DIRECT = "direct-1"
# The id that sorts first is the later post. Sequence is the order a person posted in.
LATER = "00000000-0000-0000-0000-000000000001"
EARLY = "ffffffff-ffff-ffff-ffff-ffffffffffff"
HANDSHAKE_WAIT = 8.0
APPLY_PAUSE = 1.0

STATE = None


def post(message_id, body, seq):
    return {
        "id": message_id,
        "author_id": BEA,
        "body": body,
        "seq": seq,
        "created_at": WHEN,
    }


# Arrival order is not post order. The later id is sent first.
POSTS = [post(LATER, "second", 2), post(EARLY, "first", 1)]


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.channels = 0
        self.directs = 0
        self.rooms = 0
        self.sent = 0
        self.stop = threading.Event()
        self.log = []

    def bump(self, kind):
        with self.lock:
            setattr(self, kind, getattr(self, kind) + 1)
            return getattr(self, kind)

    def counts(self):
        with self.lock:
            return self.channels, self.directs, self.rooms

    def note(self, line):
        with self.lock:
            self.log.append(line)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        STATE.note("%s %s" % (self.command, self.path))

    def _json(self, status, body):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(raw)

    def _authorized(self):
        if self.headers.get("Authorization") == "Bearer " + TOKEN:
            return True
        self._json(401, {"error": "unauthorized"})
        return False

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == "/health":
            self._json(200, {"ok": True, "media": "off"})
            return
        if not self._authorized():
            return
        if path == "/v1/live":
            self._live()
            return
        if path == "/v1/capabilities":
            self._json(200, {"features": []})
            return
        if path == "/v1/members":
            self._json(200, [
                {"user_id": ME, "display_name": "Ada", "email": "ada@example.com", "role": "member"},
                {"user_id": BEA, "display_name": "Bea", "email": "bea@example.com", "role": "member"},
            ])
            return
        if path == "/v1/channels":
            STATE.bump("channels")
            self._json(200, {"channels": []})
            return
        if path == "/v1/directs":
            STATE.bump("directs")
            self._json(200, {"directs": [{
                "id": DIRECT,
                "peer": {"user_id": BEA, "display_name": "Bea"},
                "unread": 2,
                "last_message": post(EARLY, "first", 1),
            }]})
            return
        if path == "/v1/rooms":
            STATE.bump("rooms")
            self._json(200, [])
            return
        self._json(404, {"error": "not_found"})

    def do_POST(self):
        self._json(404, {"error": "not_found"})

    do_PUT = do_DELETE = do_PATCH = do_POST

    def _live(self):
        deadline = time.time() + HANDSHAKE_WAIT
        while time.time() < deadline and not STATE.stop.is_set():
            channels, directs, rooms = STATE.counts()
            if channels >= 1 and directs >= 1 and rooms >= 1:
                break
            time.sleep(0.02)
        else:
            self._json(503, {"error": "the first list never arrived"})
            return
        time.sleep(APPLY_PAUSE)
        if STATE.stop.is_set():
            return
        key = self.headers.get("Sec-WebSocket-Key", "")
        if not key:
            self._json(400, {"error": "not a websocket"})
            return
        accept = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        ready = json.dumps({
            "type": "ready",
            "seq": 1,
            "me": {"role": "member"},
            "online": [],
            "calls": [],
        }).encode()
        self.wfile.write(_frame(ready))
        self.wfile.flush()
        self._frames(self.connection)
        self.close_connection = True

    def _frames(self, sock):
        sock.settimeout(0.5)
        pending = _posts()
        try:
            while not STATE.stop.is_set():
                if pending:
                    try:
                        sock.sendall(pending)
                        STATE.bump("sent")
                    except OSError:
                        return
                    pending = b""
                try:
                    header = _exact(sock, 2)
                except socket.timeout:
                    pending = _posts()
                    continue
                if not header:
                    return
                opcode = header[0] & 0x0F
                length = header[1] & 0x7F
                if length == 126:
                    length = int.from_bytes(_exact(sock, 2), "big")
                elif length == 127:
                    length = int.from_bytes(_exact(sock, 8), "big")
                mask = _exact(sock, 4) if header[1] & 0x80 else b""
                payload = _exact(sock, length) if length else b""
                if payload is None:
                    return
                if mask:
                    payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
                if opcode == 0x8:
                    sock.sendall(_frame(payload, 0x8))
                    return
                if opcode == 0x9:
                    sock.sendall(_frame(payload, 0xA))
                    continue
                if opcode != 0x1:
                    continue
                try:
                    kind = json.loads(payload.decode()).get("type")
                except (UnicodeDecodeError, json.JSONDecodeError, AttributeError):
                    continue
                if kind == "ping":
                    sock.sendall(_frame(json.dumps({"type": "pong"}).encode()))
        except OSError:
            return


def _posts():
    raw = b""
    for message in POSTS:
        raw += _frame(json.dumps({
            "type": "message.created",
            "conversation_id": DIRECT,
            "message": message,
        }).encode())
    return raw


def _frame(payload, opcode=0x1):
    n = len(payload)
    if n < 126:
        head = bytes([0x80 | opcode, n])
    elif n < 65536:
        head = bytes([0x80 | opcode, 126]) + n.to_bytes(2, "big")
    else:
        head = bytes([0x80 | opcode, 127]) + n.to_bytes(8, "big")
    return head + payload


def _exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def serve():
    """Listen on 127.0.0.1 and a free port. Returns (httpd, state)."""
    global STATE
    STATE = State()
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    httpd.daemon_threads = True
    threading.Thread(target=httpd.serve_forever, name="tiny-fixture", daemon=True).start()
    return httpd, STATE


def stop(httpd, state):
    state.stop.set()
    httpd.shutdown()
    httpd.server_close()


def _self_test():
    import urllib.request

    httpd, state = serve()
    port = httpd.server_address[1]
    base = "http://127.0.0.1:%d" % port

    def get(path):
        req = urllib.request.Request(base + path, headers={"Authorization": "Bearer " + TOKEN})
        with urllib.request.urlopen(req) as res:
            return res.status, json.loads(res.read().decode())

    def take(sock, buffered):
        def fill(n):
            nonlocal buffered
            while len(buffered) < n:
                chunk = sock.recv(4096)
                if not chunk:
                    raise AssertionError("socket closed mid-frame")
                buffered += chunk

        fill(2)
        mark = buffered[1] & 0x7F
        if mark == 126:
            fill(4)
            length = int.from_bytes(buffered[2:4], "big")
            header = 4
        elif mark == 127:
            fill(10)
            length = int.from_bytes(buffered[2:10], "big")
            header = 10
        else:
            length = mark
            header = 2
        fill(header + length)
        payload = buffered[header:header + length]
        return json.loads(payload.decode()), buffered[header + length:]

    try:
        assert post(LATER, "second", 2)["id"] < post(EARLY, "first", 1)["id"]
        assert [item["seq"] for item in POSTS] == [2, 1]
        status, body = get("/v1/channels")
        assert status == 200 and body["channels"] == []
        _, directs = get("/v1/directs")
        assert directs["directs"][0]["id"] == DIRECT
        assert directs["directs"][0]["last_message"]["created_at"] == WHEN
        status, rooms = get("/v1/rooms")
        assert status == 200 and rooms == []
        sock = socket.create_connection(("127.0.0.1", port), 5)
        key = base64.b64encode(b"fixture-self-test!").decode()
        sock.sendall((
            "GET /v1/live HTTP/1.1\r\n"
            "Host: 127.0.0.1:%d\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            "Sec-WebSocket-Key: %s\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "Authorization: Bearer %s\r\n"
            "\r\n" % (port, key, TOKEN)
        ).encode())
        sock.settimeout(5)
        buffered = b""
        while b"\r\n\r\n" not in buffered:
            buffered += sock.recv(1)
        head, extra = buffered.split(b"\r\n\r\n", 1)
        assert head.startswith(b"HTTP/1.1 101 "), head
        ready, extra = take(sock, extra)
        assert ready["type"] == "ready", ready
        first, extra = take(sock, extra)
        second, _extra = take(sock, extra)
        assert first["message"]["body"] == "second" and first["message"]["seq"] == 2
        assert second["message"]["body"] == "first" and second["message"]["seq"] == 1
        assert first["message"]["created_at"] == second["message"]["created_at"] == WHEN
        sock.close()
    finally:
        stop(httpd, state)
    print("fixture self-test passed")


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        _self_test()
    else:
        print("run drive.py; this file is the server it starts", file=sys.stderr)
        sys.exit(2)
