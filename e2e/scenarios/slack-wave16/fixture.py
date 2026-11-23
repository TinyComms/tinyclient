"""Loopback Slack workspace and notes page for the huddle scenario."""

import base64
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from threading import Thread
from urllib.parse import parse_qs, urlsplit

TEAM = "T01WAVE"
CHANNEL = "C0HUD"
WORKSPACE = "Wave Slack"
TOKEN = "xoxc-fixture-wave16"
ROOT = "1760000200.000200"
LIVE = "1760000600.000600"


def environment(port):
    return {
        "TINYCLIENT_SESSION_SLACK": json.dumps({
            "cookie": "xoxd-fixture-wave16", "active": TEAM,
            "teams": [{"id": TEAM, "name": WORKSPACE, "url": "https://wave-fixture.slack.com/", "token": TOKEN}],
        }),
        "TINYCLIENT_SLACK_API": f"http://127.0.0.1:{port}/",
        "TINYCLIENT_SLACK_SOCKET": f"ws://127.0.0.1:{port}/socket",
    }


def user(person_id, name):
    return {"id": person_id, "name": name.lower(), "real_name": name, "team_id": TEAM}


PEOPLE = [user("U0ME", "Taylor"), user("U0ANA", "Ana"), user("U0BEN", "Ben")]
ROOM = {"id": CHANNEL, "name": "huddle-fixture", "is_channel": True, "is_member": True, "created": 1760000000}


def huddle(ts, room, **fields):
    return {
        "type": "message", "subtype": "huddle_thread", "user": "U0ANA", "text": "", "ts": ts,
        "room": {"call_family": "huddle", "created_by": "U0ANA", "thread_root_ts": ts, **room},
        **fields,
    }


def payload(method, params, port):
    ended = huddle(
        ROOT, {"id": "R0ENDED", "has_ended": True, "date_start": 1760000200, "date_end": 1760000500,
               "participant_history": ["U0ANA", "U0BEN"], "canvas": {"file_id": "F0NOTES"}},
        reply_count=3, latest_reply="1760000400.000400", reply_users=["U0ANA", "U0BEN"],
        files=[{"id": "F0NOTES", "filetype": "quip", "title": "Launch Recap",
                "preview_plain_text": "The team agreed to launch Tuesday.",
                "permalink": f"http://127.0.0.1:{port}/notes"}],
    )
    live = huddle(LIVE, {"id": "R0LIVE", "has_ended": False, "date_start": 1760000600, "participants": ["U0ANA"]})
    answers = {
        "auth.test": {"team": WORKSPACE, "team_id": TEAM, "user": "taylor", "user_id": "U0ME"},
        "team.info": {"team": {"id": TEAM, "name": WORKSPACE}},
        "client.counts": {"channels": [{"id": CHANNEL, "last_read": LIVE, "latest": LIVE}]},
        "users.conversations": {"channels": [ROOM]},
        "conversations.info": {"channel": ROOM},
        "conversations.history": {"messages": [live, ended]},
        "users/info": {"results": PEOPLE},
    }
    if method == "users.info":
        person_id = params.get("user", [""])[0]
        person = next((person for person in PEOPLE if person["id"] == person_id), None)
        return {"ok": True, "user": person} if person else {"ok": False, "error": "user_not_found"}
    if method == "conversations.replies":
        if params.get("channel") != [CHANNEL]:
            return {"ok": False, "error": "channel_not_found"}
        if params.get("ts") == [LIVE]:
            return {"ok": True, "messages": [live]}
        if params.get("ts") != [ROOT]:
            return {"ok": False, "error": "thread_not_found"}
        replies = [
            {"type": "message", "user": "U0BEN", "ts": f"1760000{at}.000{at}", "thread_ts": ROOT, "text": text}
            for at, text in [(300, "Agenda posted"), (350, "Schedule confirmed"), (400, "Launch checklist ready")]
        ]
        return {"ok": True, "messages": [ended, *replies]}
    return {"ok": True, **answers.get(method, {})}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        method = urlsplit(self.path).path.removeprefix("/api/").removeprefix(f"/cache/{TEAM}/")
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        params = parse_qs(body.decode()) if method != "users/info" else {}
        reply = json.dumps(payload(method, params, self.server.server_port)).encode()
        self.reply(reply, "application/json")

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == "/notes":
            self.reply(b"<!doctype html><title>Huddle canvas fixture</title><h1>Huddle canvas fixture</h1>", "text/html")
        elif path == "/socket":
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
