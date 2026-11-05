"""Real loopback HTTP tests; no app, fixture, device, or shared port is touched."""
import asyncio
import json
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from lib import linkproxy


CONVERSATION = "11111111-1111-4111-8111-111111111111"
A = "22222222-2222-4222-8222-222222222222"
BEA = "33333333-3333-4333-8333-333333333333"
X = "44444444-4444-4444-8444-444444444444"
GATE = "55555555-5555-4555-8555-555555555555"


class HandshakeDeliveryGateTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.control_file = self.root / "control.json"
        self.gate = {"id": GATE, "conversation_id": CONVERSATION,
                     "device_ids": [A, BEA], "state": "paused",
                     "expires_at": time.time() + 5, "max_wait_seconds": 5}
        self.set_gate()
        self.forwarded = []
        self.tasks = set()
        self.writers = []
        self.upstream = await asyncio.start_server(self.receive, "127.0.0.1", 0)
        port = self.upstream.sockets[0].getsockname()[1]
        self.args = patch.object(linkproxy, "ARGS", SimpleNamespace(
            upstream=f"127.0.0.1:{port}", out=str(self.root), control=str(self.control_file)))
        self.args.start()
        self.gate_patch = patch.object(linkproxy, "HANDSHAKE_GATE", linkproxy.HandshakeDeliveryGate())
        self.gate_patch.start()
        self.proxy = await asyncio.start_server(self.accept, "127.0.0.1", 0)
        self.port = self.proxy.sockets[0].getsockname()[1]

    def set_gate(self):
        temporary = self.control_file.with_suffix(".tmp")
        temporary.write_text(json.dumps({"handshake_gate": self.gate}), encoding="utf-8")
        temporary.replace(self.control_file)

    def accept(self, reader, writer):
        task = asyncio.create_task(linkproxy.handle(reader, writer))
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def receive(self, reader, writer):
        try:
            first, _ = await linkproxy.read_head(reader)
            self.forwarded.append(first)
            writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
            await writer.drain()
        except asyncio.IncompleteReadError:
            pass
        finally:
            writer.close()
            await writer.wait_closed()

    async def request(self, device=A, *, path=None, method="GET"):
        reader, writer = await asyncio.open_connection("127.0.0.1", self.port)
        self.writers.append(writer)
        route = path or f"/v1/e2ee/conversations/{CONVERSATION}/handshake?after=4&device_id={device}&generation=0"
        writer.write(f"{method} {route} HTTP/1.1\r\nHost: fixture\r\nAuthorization: Bearer never-log-this\r\nContent-Length: 0\r\n\r\n".encode())
        await writer.drain()
        return reader

    async def wait_for_event(self, name):
        for _ in range(100):
            records = self.records()
            matching = [row for row in records if row.get("event") == name]
            if matching:
                return matching
            await asyncio.sleep(.01)
        self.fail(f"proxy never recorded {name}")

    def records(self):
        wire = self.root / "wire.jsonl"
        return [json.loads(line) for line in wire.read_text().splitlines()] if wire.exists() else []

    async def asyncTearDown(self):
        self.gate["state"] = "released"
        self.set_gate()
        for writer in self.writers:
            writer.close()
        for server in (self.proxy, self.upstream):
            server.close()
            await server.wait_closed()
        if self.tasks:
            await asyncio.wait_for(asyncio.gather(*self.tasks), 2)
        self.args.stop()
        self.gate_patch.stop()
        self.temp.cleanup()

    async def test_holds_both_owned_devices_before_forwarding_then_releases_exactly_once(self):
        a, bea = await self.request(), await self.request(BEA)
        await self.wait_for_event("handshake_gate_held")
        for _ in range(100):
            if len([row for row in self.records() if row.get("event") == "handshake_gate_held"]) == 2:
                break
            await asyncio.sleep(.01)
        self.assertEqual(self.forwarded, [])
        x = await self.request(X)
        self.assertIn(b"200 OK", await asyncio.wait_for(x.read(), 1))
        self.assertEqual(len(self.forwarded), 1)
        self.gate["state"] = "released"
        self.set_gate()
        responses = await asyncio.wait_for(asyncio.gather(a.read(), bea.read()), 1)
        self.assertTrue(all(b"200 OK" in response for response in responses))
        self.assertEqual(len(self.forwarded), 3)
        self.assertEqual(sum(f"device_id={A}" in row for row in self.forwarded), 1)
        self.assertEqual(sum(f"device_id={BEA}" in row for row in self.forwarded), 1)
        self.assertNotIn("never-log-this", (self.root / "wire.jsonl").read_text())

    async def test_scope_is_exact_route_method_and_query_device_not_authorization(self):
        routes = [("GET", f"/v1/e2ee/conversations/{X}/handshake?device_id={A}"),
                  ("GET", f"/v1/e2ee/conversations/{CONVERSATION}/tree?device_id={A}"),
                  ("POST", f"/v1/e2ee/conversations/{CONVERSATION}/handshake?device_id={A}"),
                  ("GET", f"/v1/e2ee/conversations/{CONVERSATION}/handshake?device_id={X}")]
        for method, path in routes:
            response = await self.request(path=path, method=method)
            self.assertIn(b"200 OK", await asyncio.wait_for(response.read(), 1))
        self.assertEqual(len(self.forwarded), 4)

    async def test_timeout_refuses_without_forwarding_or_silently_releasing(self):
        self.gate["max_wait_seconds"] = .1
        self.set_gate()
        response = await self.request()
        self.assertIn(b"503 Service Unavailable", await asyncio.wait_for(response.read(), 1))
        self.assertEqual(self.forwarded, [])
        self.assertTrue(any(row.get("event") == "handshake_gate_timeout" for row in self.records()))

    async def test_disappearing_control_cannot_release_a_held_request(self):
        self.gate["max_wait_seconds"] = .15
        self.set_gate()
        response = await self.request()
        await self.wait_for_event("handshake_gate_held")
        self.control_file.write_text("{}", encoding="utf-8")
        self.assertIn(b"503 Service Unavailable", await asyncio.wait_for(response.read(), 1))
        self.assertEqual(self.forwarded, [])

    async def test_release_with_replaced_scope_cannot_forward_a_held_request(self):
        self.gate["max_wait_seconds"] = .15
        self.set_gate()
        response = await self.request()
        await self.wait_for_event("handshake_gate_held")
        self.gate.update(state="released", device_ids=[X])
        self.set_gate()
        self.assertIn(b"503 Service Unavailable", await asyncio.wait_for(response.read(), 1))
        self.assertEqual(self.forwarded, [])

    async def test_pending_requests_are_bounded_per_owned_device(self):
        held = await self.request()
        await self.wait_for_event("handshake_gate_held")
        duplicate = await self.request()
        self.assertIn(b"503 Service Unavailable", await asyncio.wait_for(duplicate.read(), 1))
        self.assertEqual(self.forwarded, [])
        self.gate["state"] = "released"
        self.set_gate()
        self.assertIn(b"200 OK", await asyncio.wait_for(held.read(), 1))

    async def test_retry_cannot_bypass_an_armed_gate_when_control_is_missing(self):
        self.gate["max_wait_seconds"] = .15
        self.set_gate()
        held = await self.request()
        await self.wait_for_event("handshake_gate_held")
        self.control_file.write_text("{}", encoding="utf-8")
        retry = await self.request()
        self.assertIn(b"503 Service Unavailable", await asyncio.wait_for(retry.read(), 1))
        self.assertEqual(self.forwarded, [])
        self.assertIn(b"503 Service Unavailable", await asyncio.wait_for(held.read(), 1))
