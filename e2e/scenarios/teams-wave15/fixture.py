"""Loopback Teams and Exchange for the real desktop app. Every identity is invented."""

import base64
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from threading import Thread
from urllib.parse import unquote, urlsplit

TENANT = "11111111-1111-4111-8111-111111111111"
USER = "00000000-0000-4000-8000-0000000000aa"
ME = "8:orgid:" + USER
ANA = "8:orgid:00000000-0000-4000-8000-0000000000a1"
CHAT = "19:wave15@thread.v2"
TRANSCRIPT = "https://example.sharepoint.com/:t:/s/Proof/transcript"
JOIN = "https://teams.microsoft.com/l/meetup-join/19%3Ameeting_wave15%40thread.v2/0"
STAMP = "2026-01-02T09:00:00Z"
SESSION = {
    "client_id": "1fec8e78-bce4-4aaf-ab1b-5451cc387264", "user_agent": "Tiny Teams Lab",
    "oid": USER, "name": "Wave Organizer", "upn": "organizer@example.test",
    "tenants": [{"id": TENANT, "name": "Wave Teams", "refresh_token": "fixture"}],
}


def message(number, kind, content, sender=ANA, name="Ana Ruiz"):
    return {
        "id": str(number), "type": "Message", "messagetype": kind, "contenttype": "text",
        "content": content, "conversationid": CHAT, "from": sender, "imdisplayname": name,
        "composetime": datetime.fromtimestamp(number / 1000, tz=timezone.utc).isoformat(),
        "originalarrivaltime": datetime.fromtimestamp(number / 1000, tz=timezone.utc).isoformat(),
    }


@contextmanager
def teams_fixture():
    roster = [
        {"id": ME, "mri": ME, "role": "Admin", "friendlyName": "Wave Organizer"},
        {"id": ANA, "mri": ANA, "role": "Admin", "friendlyName": "Ana Ruiz"},
    ]
    recording = f"""<URIObject type="Video.2/CallRecording.1">
    <Title>Launch recording</Title><RecordingStatus status="Success"/>
    <Identifiers><Id type="callId" value="wave15"/></Identifiers>
    <RecordingContent timestamp="{STAMP}" duration="0.00:12:34">
        <item type="onedriveForBusinessVideo" uri="https://example.sharepoint.com/:v:/s/Proof/recording"/>
        <item type="onedriveForBusinessTranscript" uri="{TRANSCRIPT}"/>
    </RecordingContent></URIObject>"""
    messages = [
        message(1767344401000, "RichText/Media_CallTranscript", '{"callId":"wave15"}'),
        message(1767344400000, "RichText/Media_CallRecording", recording),
    ]

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def handle(self):
            try:
                super().handle()
            except (BrokenPipeError, ConnectionResetError):
                pass

        def respond(self, body, status=200, kind="application/json"):
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def socket_io(self):
            key = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            accept = base64.b64encode(hashlib.sha1(key.encode()).digest()).decode()
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            self.wfile.write(b"\x81\x031::")
            self.close_connection = True
            self.rfile.read()

        def handle_request(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            path = unquote(urlsplit(self.path).path)
            if path.endswith("/oauth2/v2.0/token"):
                return self.respond({"access_token": "fixture", "expires_in": 3600})
            if path == "/api/authsvc/v1.0/authz":
                claims = base64.urlsafe_b64encode(json.dumps({"skypeid": ME[2:], "exp": 1900000000}).encode()).decode().rstrip("=")
                return self.respond({
                    "tokens": {"skypeToken": f"e30.{claims}.c2ln", "expiresIn": 86400},
                    "region": "emea", "regionGtms": {
                        "chatService": base + "/chat", "middleTier": base + "/api/mt/emea",
                        "unifiedPresence": base + "/presence",
                    },
                })
            if path == "/v4/a":
                return self.respond({"socketio": base + "/", "surl": base + "/v4/f/wave15/", "connectparams": {}})
            if path == "/socket.io/1/":
                return self.respond(b"wave15:180:180:websocket", kind="text/plain")
            if path.startswith("/socket.io/1/websocket/"):
                return self.socket_io()
            if path == "/api/csa/api/v1/teams/users/me":
                return self.respond({"teams": [], "chats": [{
                    "id": CHAT, "members": roster, "title": "Wave Review", "isOneOnOne": False,
                    "isRead": True, "chatType": "chat", "lastMessage": {
                        "id": "1767344401000", "from": ANA, "imDisplayName": "Ana Ruiz",
                        "originalArrivalTime": STAMP, "messageType": "RichText/Html",
                        "content": "Launch recap",
                    },
                }]})
            if path.endswith("/users/fetchShortProfile") or "getprofiles" in path.lower():
                return self.respond([{"mri": ANA, "displayName": "Ana Ruiz", "email": "ana@example.test"}])
            if path == f"/chat/v1/threads/{CHAT}":
                return self.respond({"id": CHAT, "properties": {"threadType": "chat"}, "members": roster})
            if path == f"/chat/v1/users/ME/conversations/{CHAT}/messages":
                if self.command == "POST":
                    raw = json.loads(body)
                    mid = int(datetime.now(timezone.utc).timestamp() * 1000)
                    row = message(mid, "RichText/Html", raw["content"], ME, "Wave Organizer")
                    row["properties"] = raw.get("properties", {})
                    messages.insert(0, row)
                    return self.respond({"OriginalArrivalTime": mid}, 201)
                return self.respond({"messages": messages})
            if path.endswith("/48:calllogs/messages"):
                return self.respond({"messages": [{
                    "id": "1767344490000", "composetime": STAMP,
                    "properties": {"call-log": json.dumps({
                        "threadId": CHAT, "callDirection": "incoming", "originator": ANA,
                        "target": ME, "callState": "missed", "startTime": STAMP,
                    })},
                }]})
            if path.endswith("/mailFolders/inbox/messages"):
                return self.respond({"value": [{
                    "id": "vm1", "isRead": False, "receivedDateTime": STAMP,
                    "bodyPreview": "Please call at noon.",
                    "from": {"emailAddress": {"name": "Ana Ruiz"}},
                }]})
            if path.endswith("/driveItem/content"):
                return self.respond(b"WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n<v Ana Ruiz>Ready for launch.\n", kind="text/vtt")
            if path.endswith("/me/onlineMeetings/createOrGet"):
                return self.respond({"id": "instant", "joinWebUrl": JOIN})
            if path.endswith("/me/events") and self.command == "POST":
                return self.respond({"id": "scheduled", "onlineMeeting": {"joinUrl": JOIN}}, 201)
            if path.endswith("/calendarView"):
                return self.respond({"value": []})
            return self.respond({"messages": []} if path.endswith("/messages") else {})

        do_GET = do_POST = do_PUT = handle_request

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    base = f"http://127.0.0.1:{server.server_port}"
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield {
            "TINYCLIENT_SESSION_TEAMS": json.dumps(SESSION), "TINYCLIENT_TEAMS_LOGIN": base,
            "TINYCLIENT_TEAMS_API": base, "TINYCLIENT_TEAMS_GRAPH": base + "/graph",
            "TINYCLIENT_TEAMS_TROUTER": base, "TINYCLIENT_MEDIA_HOSTS": "127.0.0.1",
        }
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
