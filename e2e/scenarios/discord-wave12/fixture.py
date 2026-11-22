"""Loopback-only Discord gateway and account endpoints for the desktop scenario."""
import base64
import hashlib
import json
import struct
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from urllib.parse import urlsplit

TOKEN = 'MTAwMA.wave12.fixture'
CURRENT = 'wave-browser='
OTHER = 'wave-desktop='
READY = {
    'session_id': 'wave12-gateway',
    'auth_session_id_hash': CURRENT,
    'user': {'id': '1000', 'username': 'wave-account', 'global_name': 'Wave Account',
             'premium_type': 3, 'verified': True},
    'private_channels': [
        {'id': '3001', 'type': 1, 'recipients': [
            {'id': '1001', 'username': 'wave-chat', 'global_name': 'Wave Chat'},
        ]},
    ],
}


def device(session, client, os_name, place):
    return {'id_hash': session, 'approx_last_used_time': '2026-08-01T13:00:00+00:00',
            'client_info': {'platform': client, 'os': os_name, 'location': place}}


class DiscordFixture(ThreadingHTTPServer):
    def __init__(self):
        self.devices = {
            CURRENT: device(CURRENT, 'Wave Browser', 'Linux', 'Chicago'),
            OTHER: device(OTHER, 'Wave Desktop', 'Windows', 'Boston'),
        }
        self.apps = [{'id': '123', 'application': {'name': 'Wave Calendar'}}]
        self.connections = [{'id': '234', 'type': 'spotify', 'name': 'Wave Music'}]
        super().__init__(('127.0.0.1', 0), Handler)


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def reply(self, status, body=None):
        data = json.dumps(body).encode() if body is not None else b''
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(data)
        self.close_connection = True

    def do_GET(self):
        path = urlsplit(self.path).path
        if self.headers.get('Upgrade', '').lower() == 'websocket':
            self.gateway()
        elif path == '/auth/sessions':
            self.reply(200, {'user_sessions': list(self.server.devices.values())})
        elif path == '/oauth2/tokens':
            self.reply(200, self.server.apps)
        elif path == '/users/@me/connections':
            self.reply(200, self.server.connections)
        elif path.startswith('/users/@me/settings-proto/'):
            self.reply(200, {'settings': ''})
        elif path == '/channels/3001/messages':
            self.reply(200, [])
        else:
            self.reply(404, {'message': 'Fixture route not found'})

    def do_POST(self):
        data = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if self.path != '/auth/sessions/logout':
            self.reply(404, {'message': 'Fixture route not found'})
            return
        sessions = json.loads(data)['session_id_hashes']
        if CURRENT in sessions or not sessions:
            self.reply(400, {'message': 'Expected another device'})
            return
        for session in sessions:
            self.server.devices.pop(session, None)
        self.reply(204)

    def do_DELETE(self):
        if self.path == '/oauth2/tokens/123':
            self.server.apps.clear()
        elif self.path == '/users/@me/connections/spotify/234':
            self.server.connections.clear()
        else:
            self.reply(404, {'message': 'Fixture route not found'})
            return
        self.reply(204)

    def send_frame(self, opcode, data):
        if len(data) < 126:
            header = bytes((0x80 | opcode, len(data)))
        else:
            header = struct.pack('!BBH', 0x80 | opcode, 126, len(data))
        self.wfile.write(header + data)

    def read_frame(self):
        header = self.rfile.read(2)
        if len(header) != 2:
            return 8, b''
        opcode, length = header[0] & 0x0f, header[1] & 0x7f
        if length in (126, 127):
            length = int.from_bytes(self.rfile.read(2 if length == 126 else 8), 'big')
        mask = self.rfile.read(4)
        payload = self.rfile.read(length)
        return opcode, bytes(value ^ mask[index % 4] for index, value in enumerate(payload))

    def gateway(self):
        key = self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
        accept = base64.b64encode(hashlib.sha1(key.encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.end_headers()
        self.close_connection = True

        def send(data):
            self.send_frame(1, json.dumps(data).encode())

        try:
            send({'op': 10, 'd': {'heartbeat_interval': 30000}})
            while True:
                opcode, payload = self.read_frame()
                if opcode == 8:
                    return
                if opcode == 9:
                    self.send_frame(10, payload)
                if opcode != 1:
                    continue
                event = json.loads(payload)
                if event['op'] == 1:
                    send({'op': 11, 'd': None})
                if event['op'] == 2:
                    send({'op': 0, 't': 'READY', 's': 1, 'd': READY})
                    send({'op': 0, 't': 'READY_SUPPLEMENTAL', 's': 2, 'd': {}})
        except (OSError, ValueError):
            return


@contextmanager
def discord_fixture():
    with DiscordFixture() as server:
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            thread.join()
