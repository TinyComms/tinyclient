#!/usr/bin/env python3
"""A Tiny server small enough to show the unread reset.

The real channel list has no last_read. chatd fetches it once as the account starts, then again
when the live socket's first ready arrives. This server answers the first fetch with unread 1 and
every later fetch with unread 0, the same last message, and still no cursor. It holds the
websocket handshake until that first fetch has been applied, so the second fetch is the one that
used to wipe the badge.

Standard library only. `python fixture.py --self-test` checks the two answers without chatd.
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
# How long the handshake waits for the first list, inside the client's 10s connect limit.
HANDSHAKE_WAIT = 8.0
# After the first list's last HTTP response, before ready: long enough for that publish to apply.
APPLY_PAUSE = 1.0

STATE = None


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.channels = 0
        self.directs = 0
        self.rooms = 0
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


def message(state, message_id):
    return {
        "id": message_id,
        "author_id": BEA,
        "body": "still unread",
        "created_at": state.created,
    }


def lists(state, unread):
    """The channel has no cursor, which is what the server sends. The direct omits its cursor
    too, so the inbox (which lists directs, not channels) shows the same decision."""
    channels = {
        "channels": [{
            "id": "night-watch",
            "name": "Night Watch",
            "topic": "The late shift",
            "position": 1,
            "unread": unread,
            "last_message": message(state, "00000000-0000-4000-8000-000000000001"),
        }]
    }
    directs = {
        "directs": [{
            "id": "direct-1",
            "peer": {"user_id": BEA, "display_name": "Bea"},
            "unread": unread,
            "last_message": message(state, "00000000-0000-4000-8000-000000000002"),
        }]
    }
    return channels, directs


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
            n = STATE.bump("channels")
            channels, _ = lists(STATE, 1 if n == 1 else 0)
            self._json(200, channels)
            return
        if path == "/v1/directs":
            # Channels are fetched first in a publish, so that count is which publish this is.
            generation = STATE.counts()[0]
            STATE.bump("directs")
            _, directs = lists(STATE, 1 if generation == 1 else 0)
            self._json(200, directs)
            return
        if path == "/v1/rooms":
            STATE.bump("rooms")
            # An empty list, not a wrapped object: chatd reads this body as the array itself.
            # 404 would also do; an empty 200 keeps the publish on the path that stores unreads.
            self._json(200, [])
            return
        self._json(404, {"error": "not_found"})

    def do_POST(self):
        self._json(404, {"error": "not_found"})

    do_PUT = do_DELETE = do_PATCH = do_POST

    def _live(self):
        # Stay in the handshake until the account's first publish can have been stored. ready
        # is what makes chatd publish the list again, and that second list is the reset.
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
        try:
            while not STATE.stop.is_set():
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
        status, body = get("/v1/channels")
        assert status == 200 and body["channels"][0]["unread"] == 1, body
        assert "last_read" not in body["channels"][0]
        _, directs = get("/v1/directs")
        assert directs["directs"][0]["unread"] == 1
        assert "last_read" not in directs["directs"][0]
        status, rooms = get("/v1/rooms")
        assert status == 200 and rooms == []
        # ready is withheld until the first list is done, then the next list is unread 0.
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
        _, body = get("/v1/channels")
        assert body["channels"][0]["unread"] == 0, body
        assert body["channels"][0]["last_message"]["created_at"] == state.created
        _, directs = get("/v1/directs")
        assert directs["directs"][0]["unread"] == 0
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
