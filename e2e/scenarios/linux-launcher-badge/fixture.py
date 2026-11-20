#!/usr/bin/env python3
"""A Tiny server with one unread direct, and a way to read it from another device.

The first list says Bea's direct has unread 1 and no cursor, which is what the real server
sends. `mark_read()` says the direct was read elsewhere: a `read.updated` frame with my own
user id over the live socket, the same tell a second device sends. Every later list says
unread 0 with the cursor set, so a reconnect cannot resurrect the badge.

Standard library only. `python fixture.py --self-test` checks the answers and the event
without chatd.
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
DIRECT = "direct-1"
DIRECT_MESSAGE = "00000000-0000-4000-8000-000000000002"
# How long the handshake waits for the first list, inside the client's 10s connect limit.
HANDSHAKE_WAIT = 8.0

STATE = None


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.channels = 0
        self.directs = 0
        self.rooms = 0
        self.read = False
        self.pending = []
        self.created = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
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

    def mark_read(self):
        """The read tell a second device sends: my own user id, the direct's last message."""
        with self.lock:
            self.read = True
            self.pending.append(json.dumps({
                "type": "read.updated",
                "conversation_id": DIRECT,
                "message_id": DIRECT_MESSAGE,
                "user_id": ME,
            }))

    def take_pending(self):
        with self.lock:
            pending, self.pending = self.pending, []
            return pending


def message(state, message_id):
    return {
        "id": message_id,
        "author_id": BEA,
        "body": "the launcher should say 1",
        "created_at": state.created,
    }


def lists(state):
    """Bea's direct is unread until it was read, then read through with its cursor set."""
    unread = 0 if state.read else 1
    direct = {
        "id": DIRECT,
        "peer": {"user_id": BEA, "display_name": "Bea"},
        "unread": unread,
        "last_message": message(state, DIRECT_MESSAGE),
    }
    if state.read:
        direct["last_read"] = DIRECT_MESSAGE
    channels = {
        "channels": [{
            "id": "night-watch",
            "name": "Night Watch",
            "topic": "The late shift",
            "position": 1,
            "unread": 0,
            "last_message": message(state, "00000000-0000-4000-8000-000000000001"),
        }]
    }
    return channels, {"directs": [direct]}


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
        # A fresh ready asks /health with no bearer, only to see if call audio is up.
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
            channels, _ = lists(STATE)
            self._json(200, channels)
            return
        if path == "/v1/directs":
            STATE.bump("directs")
            _, directs = lists(STATE)
            self._json(200, directs)
            return
        if path == "/v1/rooms":
            STATE.bump("rooms")
            # An empty list, not a wrapped object: chatd reads this body as the array itself.
            self._json(200, [])
            return
        self._json(404, {"error": "not_found"})

    def do_POST(self):
        self._json(404, {"error": "not_found"})

    do_PUT = do_DELETE = do_PATCH = do_POST

    def _live(self):
        # Stay in the handshake until the account's first list has been asked for, so the
        # store is filled before ready makes the window publish it.
        deadline = time.time() + HANDSHAKE_WAIT
        while time.time() < deadline and not STATE.stop.is_set():
            channels, directs, rooms = STATE.counts()
            if channels >= 1 and directs >= 1 and rooms >= 1:
                break
            time.sleep(0.02)
        else:
            self._json(503, {"error": "the first list never arrived"})
            return
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
        try:
            while not STATE.stop.is_set():
                for frame in STATE.take_pending():
                    sock.sendall(_frame(frame.encode()))
                try:
                    header = _exact(sock, 2)
                except socket.timeout:
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


def _read_frame(sock):
    """One frame from the server: (opcode, payload), or None when it does not come in time."""
    header = b""
    while len(header) < 2:
        chunk = sock.recv(2 - len(header))
        if not chunk:
            return None
        header += chunk
    opcode = header[0] & 0x0F
    length = header[1] & 0x7F
    if length == 126:
        length = int.from_bytes(_exact(sock, 2), "big")
    elif length == 127:
        length = int.from_bytes(_exact(sock, 8), "big")
    payload = b""
    while len(payload) < length:
        payload += sock.recv(length - len(payload))
    return opcode, payload


def _self_test():
    import urllib.request

    httpd, state = serve()
    port = httpd.server_address[1]
    base = "http://127.0.0.1:%d" % port

    def get(path):
        req = urllib.request.Request(base + path, headers={"Authorization": "Bearer " + TOKEN})
        with urllib.request.urlopen(req) as res:
            return res.status, json.loads(res.read().decode())

    try:
        _, body = get("/v1/directs")
        assert body["directs"][0]["unread"] == 1, body
        assert "last_read" not in body["directs"][0], body
        status, rooms = get("/v1/rooms")
        assert status == 200 and rooms == [], rooms
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
        frame = extra
        while len(frame) < 2:
            frame += sock.recv(2 - len(frame))
        length = frame[1] & 0x7F
        payload = frame[2:]
        while len(payload) < length:
            payload += sock.recv(length - len(payload))
        assert json.loads(payload.decode())["type"] == "ready", payload
        # The read tell goes out over the socket, to the client that is listening here.
        state.mark_read()
        seen = None
        try:
            while seen is None:
                got = _read_frame(sock)
                if got and got[0] == 0x1:
                    body = json.loads(got[1].decode())
                    if body.get("type") == "read.updated":
                        seen = body
        except socket.timeout:
            pass
        assert seen and seen["user_id"] == ME and seen["conversation_id"] == DIRECT, seen
        sock.close()
        _, body = get("/v1/directs")
        assert body["directs"][0]["unread"] == 0, body
        assert body["directs"][0]["last_read"] == DIRECT_MESSAGE, body
    finally:
        stop(httpd, state)
    print("fixture self-test passed")


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        _self_test()
    else:
        print("run drive.py; this file is the server it starts", file=sys.stderr)
        sys.exit(2)
