"""Loopback Discord, voice and Go Live endpoints for the desktop scenario."""

import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from socket import AF_INET, SOCK_DGRAM, socket
import struct
import subprocess
import time
from threading import Event, Thread
from urllib.parse import urlsplit

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
TOKEN = 'MTAwMA.wave15.fixture'
CHANNEL = '3001'
OWNER = '1001'
STREAM = f'call:{CHANNEL}:{OWNER}'
MODE = 'aead_aes256_gcm_rtpsize'
KEY = bytes(range(32))
READY = {
    'session_id': 'wave15-gateway',
    'user': {'id': '1000', 'username': 'wave-account', 'global_name': 'Wave Account'},
    'private_channels': [
        {'id': CHANNEL, 'type': 1, 'recipients': [
            {'id': OWNER, 'username': 'wave-chat', 'global_name': 'Wave Chat'},
        ]},
    ],
}


def tone_frames():
    ogg = subprocess.run(
        ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i',
         'sine=frequency=440:sample_rate=48000:duration=4', '-af', 'volume=3',
         '-ac', '2', '-c:a', 'libopus', '-frame_duration', '20', '-f', 'ogg', 'pipe:1'],
        capture_output=True, check=True).stdout
    packets, packet = [], bytearray()
    while ogg:
        if ogg[:4] != b'OggS':
            raise ValueError('FFmpeg did not return Ogg pages')
        sizes = ogg[27:27 + ogg[26]]
        offset = 27 + len(sizes)
        for size in sizes:
            packet.extend(ogg[offset:offset + size])
            offset += size
            if size < 255:
                packets.append(bytes(packet))
                packet.clear()
        ogg = ogg[offset:]
    return packets[2:-1]


def send_tone(udp, client, frames, done):
    cipher = AESGCM(KEY)
    deadline = time.monotonic()
    number = 0
    while not done.is_set():
        for frame in frames:
            if done.wait(max(0, deadline - time.monotonic())):
                return
            header = struct.pack('!BBHII', 0x80, 120, number & 0xffff,
                                 (number * 960) & 0xffffffff, 2222)
            counter = struct.pack('!I', number)
            sealed = cipher.encrypt(counter + bytes(8), frame, header)
            try:
                udp.sendto(header + sealed + counter, client)
            except OSError:
                return
            deadline += 0.020
            number += 1


class MediaFixture(ThreadingHTTPServer):
    def __init__(self):
        self.voice_ready = Event()
        self.frames = tone_frames()
        super().__init__(('127.0.0.1', 0), MediaHandler)


class MediaHandler(BaseHTTPRequestHandler):
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
        elif path.startswith('/users/@me/settings-proto/'):
            self.reply(200, {'settings': ''})
        elif path == f'/channels/{CHANNEL}/messages':
            self.reply(200, [])
        else:
            self.reply(404, {'message': 'Fixture route not found'})

    def send_frame(self, opcode, data):
        size = len(data)
        if size < 126:
            header = bytes((0x80 | opcode, size))
        else:
            header = struct.pack('!BBH', 0x80 | opcode, 126, size)
        self.wfile.write(header + data)

    def read_frame(self):
        header = self.rfile.read(2)
        if len(header) != 2:
            return 8, b''
        opcode, size = header[0] & 0x0f, header[1] & 0x7f
        if size in (126, 127):
            size = int.from_bytes(self.rfile.read(2 if size == 126 else 8), 'big')
        mask = self.rfile.read(4)
        if len(mask) != 4:
            raise ValueError('Expected a masked WebSocket frame')
        payload = self.rfile.read(size)
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
        try:
            if urlsplit(self.path).query == 'v=8':
                self.voice_gateway()
            else:
                self.main_gateway()
        except (OSError, ValueError):
            return

    def send(self, op, body, **extra):
        self.send_frame(1, json.dumps(dict(op=op, d=body, **extra)).encode())

    def receive(self):
        while True:
            opcode, payload = self.read_frame()
            if opcode == 8:
                return None
            if opcode == 9:
                self.send_frame(10, payload)
            if opcode == 1:
                return json.loads(payload)

    def main_gateway(self):
        seq = 0
        endpoint = f'127.0.0.1:{self.server.server_port}'
        joined = False

        def dispatch(kind, body):
            nonlocal seq
            seq += 1
            self.send(0, body, t=kind, s=seq)

        call = {'channel_id': CHANNEL, 'region': 'us-east', 'ringing': []}
        owner = {'channel_id': CHANNEL, 'user_id': OWNER, 'session_id': 'peer-test'}
        self.send(10, {'heartbeat_interval': 30000})
        while (message := self.receive()) is not None:
            op, data = message['op'], message['d']
            if op == 1:
                self.send(11, None)
            elif op == 2:
                dispatch('READY', READY)
                dispatch('READY_SUPPLEMENTAL', {})
                dispatch('CALL_CREATE', call)
                dispatch('VOICE_STATE_UPDATE', owner)
            elif op == 4 and data.get('channel_id') == CHANNEL and not joined:
                joined = True
                dispatch('VOICE_STATE_UPDATE', {'channel_id': CHANNEL, 'user_id': '1000',
                                                'session_id': 'voice-test'})
                dispatch('VOICE_SERVER_UPDATE', {'channel_id': CHANNEL, 'token': 'voice-token',
                                                 'endpoint': endpoint})
                if not self.server.voice_ready.wait(8):
                    raise ValueError('The main voice connection never opened')
                dispatch('VOICE_STATE_UPDATE', dict(owner, self_stream=True))
            elif op == 20 and data.get('stream_key') == STREAM:
                dispatch('STREAM_CREATE', {'stream_key': STREAM, 'rtc_server_id': '900',
                                           'rtc_channel_id': CHANNEL})
                dispatch('STREAM_SERVER_UPDATE', {'stream_key': STREAM, 'token': 'watch-token',
                                                  'endpoint': endpoint})

    def voice_gateway(self):
        identify = self.receive()
        if identify is None or identify['op'] != 0:
            return
        watch = identify['d']['token'] == 'watch-token'
        done = Event()
        started = False
        with socket(AF_INET, SOCK_DGRAM) as udp:
            udp.bind(('127.0.0.1', 0))
            udp.settimeout(8)
            self.send(8, {'v': 8, 'heartbeat_interval': 5000})
            self.send(2, {'ssrc': 1111, 'ip': '127.0.0.1',
                          'port': udp.getsockname()[1], 'modes': [MODE]})
            request, client = udp.recvfrom(128)
            if len(request) != 74 or request[:2] != b'\x00\x01':
                raise ValueError('Expected voice IP discovery')
            udp.sendto(struct.pack('!HHI64sH', 2, 70, 1111, b'127.0.0.1', client[1]), client)
            select = self.receive()
            if select is None or select['op'] != 1:
                return
            self.send(4, {'mode': MODE, 'secret_key': list(KEY), 'dave_protocol_version': 1})
            try:
                while (message := self.receive()) is not None:
                    op = message['op']
                    if op == 3:
                        self.send(6, message['d'])
                    elif op == 5 and not watch:
                        self.server.voice_ready.set()
                    elif op == 15 and watch and not started:
                        self.send(12, {'user_id': OWNER, 'audio_ssrc': 2222, 'streams': []})
                        Thread(target=send_tone, args=(udp, client, self.server.frames, done),
                               daemon=True).start()
                        started = True
            finally:
                done.set()
