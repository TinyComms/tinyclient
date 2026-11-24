"""Loopback Slack workspace for the live huddle scenario: one channel, whose huddle starts while
the app is signed in. The driver has this workspace say so on its socket, the way Slack posts the
huddle's message, and later update the same message as one person leaves and as it ends."""

import base64
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from queue import Empty, Queue
from socket import timeout as TimedOut
from threading import Thread
from urllib.parse import parse_qs, urlsplit

TEAM = "T2LIVE"
CHANNEL = "C2STANDUP"
NAME = "standup"
WORKSPACE = "Live Slack"
TOKEN = "xoxc-fixture-huddle-live"
# An older message of the channel, then the huddle's own message.
NOTE = "1760000500.000500"
HUDDLE = "1760000600.000600"
ANA, BEN = "U0ANA", "U0BEN"


def environment(port):
    return {
        "TINYCLIENT_SESSION_SLACK": json.dumps({
            "cookie": "xoxd-fixture-huddle-live", "active": TEAM,
            "teams": [{"id": TEAM, "name": WORKSPACE, "url": "https://live-fixture.slack.com/", "token": TOKEN}],
        }),
        "TINYCLIENT_SLACK_API": f"http://127.0.0.1:{port}/",
        "TINYCLIENT_SLACK_SOCKET": f"ws://127.0.0.1:{port}/socket",
    }


def user(person_id, name):
    return {"id": person_id, "name": name.lower(), "real_name": name, "team_id": TEAM}


PEOPLE = [user("U0ME", "Taylor"), user(ANA, "Ana"), user(BEN, "Ben")]
ROOM = {"id": CHANNEL, "name": NAME, "is_channel": True, "is_member": True, "created": 1760000000}
NOTE_MESSAGE = {"type": "message", "user": BEN, "text": "Standup in five minutes.", "ts": NOTE}


def huddle_message(who, ended=False):
    """The huddle's message. Its room object says who is in the huddle now, and whether it is over."""
    room = {"id": "R2LIVE", "call_family": "huddle", "created_by": ANA, "thread_root_ts": HUDDLE,
            "channels": [CHANNEL], "date_start": 1760000600, "participants": who, "has_ended": ended}
    if ended:
        room["date_end"] = 1760000900
    return {"type": "message", "subtype": "huddle_thread", "user": ANA, "text": "", "ts": HUDDLE,
            "channel": CHANNEL, "no_notifications": True, "room": room}


def huddle_started():
    """The socket as the huddle starts: its message, with two people in it already."""
    return huddle_message([ANA, BEN])


def huddle_updated(who, ended=False):
    """The socket as people come and go, or as the huddle ends: the same message, changed."""
    return {"type": "message", "subtype": "message_changed", "hidden": True, "channel": CHANNEL,
            "ts": "1760000700.000000", "event_ts": "1760000700.000000", "message": huddle_message(who, ended)}


# Frames waiting for chatd's socket, and the huddle as the last event left it: what a page of the
# channel's history then says.
EVENTS = Queue()
HUDDLE_NOW = {"who": None, "ended": False}


def payload(method, params):
    if HUDDLE_NOW["who"] is None:
        messages = [NOTE_MESSAGE]
    else:
        messages = [NOTE_MESSAGE, huddle_message(HUDDLE_NOW["who"], HUDDLE_NOW["ended"])]
    answers = {
        "auth.test": {"team": WORKSPACE, "team_id": TEAM, "user": "taylor", "user_id": "U0ME"},
        "team.info": {"team": {"id": TEAM, "name": WORKSPACE}},
        "client.counts": {"channels": [{"id": CHANNEL, "last_read": NOTE, "latest": NOTE}]},
        "users.conversations": {"channels": [ROOM]},
        "conversations.info": {"channel": ROOM},
        "conversations.history": {"has_more": False, "messages": messages},
        "users/info": {"results": PEOPLE},
    }
    if method == "users.info":
        person_id = params.get("user", [""])[0]
        person = next((person for person in PEOPLE if person["id"] == person_id), None)
        return {"ok": True, "user": person} if person else {"ok": False, "error": "user_not_found"}
    if method == "conversations.replies":
        return {"ok": False, "error": "thread_not_found"}
    return {"ok": True, **answers.get(method, {})}


def frame(text):
    """One websocket text frame, as a server sends it: unmasked, with its own length."""
    payload = text.encode()
    if len(payload) < 126:
        head = bytes((0x81, len(payload)))
    elif len(payload) < 65_536:
        head = bytes((0x81, 0x7E, len(payload) >> 8, len(payload) & 0xFF))
    else:
        head = bytes((0x81, 0x7F)) + len(payload).to_bytes(8, "big")
    return head + payload


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        path = urlsplit(self.path).path
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if path == "/socket-event":
            # The driver has the workspace say something: queued for chatd's socket.
            event = json.loads(body)
            room = event.get("room") or event.get("message", {}).get("room")
            if room:
                HUDDLE_NOW.update(who=room["participants"], ended=room["has_ended"])
            EVENTS.put(event)
            return self.reply(b'{"ok": true}', "application/json")
        # The web API is asked in a form; the edge cache and nothing else here in JSON.
        method = path.removeprefix("/api/").removeprefix(f"/cache/{TEAM}/")
        params = parse_qs(body.decode()) if path.startswith("/api/") else {}
        reply = json.dumps(payload(method, params)).encode()
        self.reply(reply, "application/json")

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == "/socket":
            self.slack_socket()
        else:
            self.send_error(404)

    def reply(self, body, mime):
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def slack_socket(self):
        self.close_connection = True
        key = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        accept = base64.b64encode(hashlib.sha1(key.encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.write(frame('{"type":"hello"}'))
        # What chatd sends (presence subscriptions, pings) is drained and not read. What the
        # driver queued goes out, as Slack's own frames would.
        self.connection.settimeout(0.25)
        try:
            while True:
                try:
                    if not self.connection.recv(4096):
                        return
                except TimedOut:
                    pass
                while True:
                    try:
                        event = EVENTS.get_nowait()
                    except Empty:
                        break
                    self.connection.sendall(frame(json.dumps(event)))
        except OSError:
            pass


@contextmanager
def slack_fixture():
    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            worker.join()
