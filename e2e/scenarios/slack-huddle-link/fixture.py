"""Loopback Slack workspace for the huddle link scenario: a channel with a live huddle, and
another whose one message carries the huddle's address, as someone pasted it."""

import base64
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from threading import Thread
from urllib.parse import parse_qs, urlsplit

TEAM = "T2HUDDLE"
CHANNEL = "C2STANDUP"
OTHER = "C2TOWN"
WORKSPACE = "Link Slack"
TOKEN = "xoxc-fixture-huddle-link"
LIVE = "1760000600.000600"
PASTED = "1760000700.000700"
# What Slack's own "Copy huddle link" gives for the channel's huddle.
LINK = f"https://app.slack.com/huddle/{TEAM}/{CHANNEL}"


def environment(port):
    return {
        "TINYCLIENT_SESSION_SLACK": json.dumps({
            "cookie": "xoxd-fixture-huddle-link", "active": TEAM,
            "teams": [{"id": TEAM, "name": WORKSPACE, "url": "https://link-fixture.slack.com/", "token": TOKEN}],
        }),
        "TINYCLIENT_SLACK_API": f"http://127.0.0.1:{port}/",
        "TINYCLIENT_SLACK_SOCKET": f"ws://127.0.0.1:{port}/socket",
    }


def user(person_id, name):
    return {"id": person_id, "name": name.lower(), "real_name": name, "team_id": TEAM}


PEOPLE = [user("U0ME", "Taylor"), user("U0ANA", "Ana"), user("U0BEN", "Ben")]
ROOM = {"id": CHANNEL, "name": "huddle-fixture", "is_channel": True, "is_member": True, "created": 1760000000}
TOWN = {"id": OTHER, "name": "town-square", "is_channel": True, "is_member": True, "created": 1760000000}
LIVE_HUDDLE = {
    "type": "message", "subtype": "huddle_thread", "user": "U0ANA", "text": "", "ts": LIVE,
    "room": {"call_family": "huddle", "created_by": "U0ANA", "thread_root_ts": LIVE, "id": "R2LIVE",
             "has_ended": False, "date_start": 1760000600, "participants": ["U0ANA"]},
}
# As Slack's own client sends a pasted address: the text, and the rich text block that makes it
# a link. The block is what chatd draws the link from; the bare text would stay plain.
PASTED_LINK = {
    "type": "message", "user": "U0BEN", "text": LINK, "ts": PASTED,
    "blocks": [{"type": "rich_text", "block_id": "B2PASTED", "elements": [
        {"type": "rich_text_section", "elements": [{"type": "link", "url": LINK, "text": LINK}]},
    ]}],
}
ROOMS = {CHANNEL: ROOM, OTHER: TOWN}
HISTORY = {CHANNEL: [LIVE_HUDDLE], OTHER: [PASTED_LINK]}


def payload(method, params):
    asked = params.get("channel", [""])[0]
    answers = {
        "auth.test": {"team": WORKSPACE, "team_id": TEAM, "user": "taylor", "user_id": "U0ME"},
        "team.info": {"team": {"id": TEAM, "name": WORKSPACE}},
        "client.counts": {"channels": [
            {"id": CHANNEL, "last_read": LIVE, "latest": LIVE},
            {"id": OTHER, "last_read": PASTED, "latest": PASTED},
        ]},
        "users.conversations": {"channels": [ROOM, TOWN]},
        "conversations.info": {"channel": ROOMS.get(asked, ROOM)},
        "conversations.history": {"has_more": False, "messages": HISTORY.get(asked, [])},
    }
    if method == "users.info":
        person_id = params.get("user", [""])[0]
        person = next((person for person in PEOPLE if person["id"] == person_id), None)
        return {"ok": True, "user": person} if person else {"ok": False, "error": "user_not_found"}
    if method == "conversations.replies":
        return {"ok": False, "error": "thread_not_found"}
    return {"ok": True, **answers.get(method, {})}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        path = urlsplit(self.path).path
        method = path.removeprefix("/api/").removeprefix(f"/cache/{TEAM}/")
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        # The web API is asked in a form; the edge cache and nothing else here in JSON.
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
        hello = b'{"type":"hello"}'
        self.wfile.write(bytes((0x81, len(hello))) + hello)
        self.connection.settimeout(90)
        try:
            while self.connection.recv(4096):
                pass
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
