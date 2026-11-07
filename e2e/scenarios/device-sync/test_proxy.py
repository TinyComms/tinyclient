import asyncio
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from lib import linkproxy
from lib.linkproxy import HandshakeFault


class HandshakeFaultTest(unittest.TestCase):
    def test_only_one_matching_get_fails_and_other_devices_are_untouched(self):
        fault = HandshakeFault()
        control = {"fail_handshake_once_for": "new-device"}
        path = "/v1/e2ee/conversations/self/handshake?after=0&device_id=new-device"
        self.assertFalse(fault.take("GET", path.replace("new-device", "old-device"), control))
        self.assertFalse(fault.take("POST", path, control))
        self.assertTrue(fault.take("GET", path, control))
        self.assertFalse(fault.take("GET", path, control))

    def test_disabled_fault_does_not_consume_its_one_failure(self):
        fault = HandshakeFault()
        path = "/v1/e2ee/conversations/self/handshake?device_id=new-device"
        self.assertFalse(fault.take("GET", path, {}))
        self.assertTrue(fault.take("GET", path, {"fail_handshake_once_for": "new-device"}))

    def test_self_handshake_fault_ignores_a_regular_conversation(self):
        fault = HandshakeFault()
        control = {"fail_handshake_once_for": "new-device", "fail_handshake_conversation": "self"}
        self.assertFalse(fault.take("GET", "/v1/e2ee/conversations/dm/handshake?device_id=new-device", control))
        self.assertTrue(fault.take("GET", "/v1/e2ee/conversations/self/handshake?device_id=new-device", control))


class UpstreamConnectionTest(unittest.IsolatedAsyncioTestCase):
    class Writer:
        def __init__(self):
            self.data = bytearray()
        def write(self, data):
            self.data.extend(data)
        async def drain(self):
            pass
        def close(self):
            pass

    def reader(self, data):
        reader = asyncio.StreamReader()
        reader.feed_data(data)
        reader.feed_eof()
        return reader

    def duplicate_endpoint(self):
        error = OSError("fixture ConnectEx duplicate endpoint")
        error.winerror = 52
        return error

    async def forward(self, attempts, response=None, request=None, after_forward_error=None):
        client = self.reader(request or b"GET /health HTTP/1.1\r\nHost: fixture\r\n\r\n")
        target = self.reader(response or b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
        if after_forward_error:
            target.readuntil = AsyncMock(side_effect=after_forward_error)
        client_out, upstream_out = self.Writer(), self.Writer()
        connect = AsyncMock(side_effect=attempts + [(target, upstream_out)])
        with patch.object(linkproxy, "ARGS", SimpleNamespace(upstream="127.0.0.1:1")), \
             patch.object(linkproxy, "control", return_value={}), patch.object(linkproxy, "log"), \
             patch.object(linkproxy.asyncio, "open_connection", connect), \
             patch.object(linkproxy.asyncio, "sleep", AsyncMock()):
            await linkproxy.handle(client, client_out)
        return connect, bytes(client_out.data), bytes(upstream_out.data)

    async def test_windows_pre_request_endpoint_collision_retries_without_replaying_http(self):
        connect, response, request = await self.forward([self.duplicate_endpoint()])
        self.assertIn(b"200 OK", response)
        self.assertEqual(connect.await_count, 2)
        self.assertEqual(request.count(b"GET /health"), 1)

    async def test_connection_retries_are_bounded(self):
        connect, response, request = await self.forward([self.duplicate_endpoint() for _ in range(4)])
        self.assertEqual(connect.await_count, 3)
        self.assertEqual((response, request), (b"", b""))

    async def test_other_connection_failures_are_not_hidden(self):
        connect, response, request = await self.forward([ConnectionRefusedError("fixture is stopped")])
        self.assertEqual(connect.await_count, 1)
        self.assertEqual((response, request), (b"", b""))

    async def test_error_after_forwarding_never_replays_a_post(self):
        posted = b"POST /v1/e2ee/links HTTP/1.1\r\nHost: fixture\r\nContent-Length: 7\r\n\r\n{\"x\":1}"
        connect, response, request = await self.forward([], request=posted,
                                                       after_forward_error=self.duplicate_endpoint())
        self.assertEqual(connect.await_count, 1)
        self.assertEqual(request, posted)
        self.assertEqual(response, b"")
