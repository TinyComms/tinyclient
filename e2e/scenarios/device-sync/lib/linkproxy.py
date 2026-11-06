"""Wire observer and fault injector in front of the throwaway tinyserver.

Every device in the device-link proof talks to the server through this proxy, so the proof can
say, from the wire and not from the app's word, what happened: which link routes were called and
with what status, that the archive was uploaded once and downloaded, that both sides DELETEd the
link. It never logs a body. It is also the only place a fault can be put between two devices
without changing either app:

  tamper_archive   flip one byte of the archive as the new device downloads it
                   (GET /v1/e2ee/links/{id}/archive); the new device must reject it.
  swap_old_pub     replace the old device's ephemeral public key (the first {from:"old"} message)
                   with one the proxy made: a server in the middle. The check codes then differ
                   (or the channel fails to open); the link must abort.
  drop_link_routes answer 404 to every /v1/e2ee/links route (an "older server"), to see the
                   client fall back to the paste-the-device-code flow.

The mode is read from a JSON control file on every request, so a driver flips it between steps:
  {"tamper_archive": true, "tamper_offset": 70000, "swap_old_pub": false, "drop_link_routes": false}

Output: <out>/wire.jsonl (one line per request: time, conn, method, path, status, bytes) and
<out>/observed-links.json (per link id: the old device's public key as sent and, under
swap_old_pub, what the proxy put in its place). Public keys only; never data of a sealed message.

Usage: python linkproxy.py   (not -I: `cryptography` is in the user site-packages) --listen 0.0.0.0:8094 --upstream 127.0.0.1:8093 --out DIR --control FILE
Stdlib + `cryptography` (already on the coordinator; only swap_old_pub needs it).
"""

import argparse
import asyncio
import base64
import itertools
import json
import math
import os
import re
import sys
import time
import uuid
from urllib.parse import parse_qs, urlsplit

LINK_RE = re.compile(r"^/v1/e2ee/links(?:/([A-Za-z0-9_-]+))?(/messages|/archive)?(?:\?.*)?$")
HEAD_LIMIT = 64 * 1024
SMALL_BODY = 64 * 1024  # bodies the proxy may need to read whole (link messages)

conn_ids = itertools.count(1)
ARGS = None
observed = {}


class HandshakeFault:
    """One transient failure for a named test device; never delays other devices."""
    def __init__(self):
        self.used = set()

    def take(self, method, path, ctl):
        target = ctl.get("fail_handshake_once_for")
        request = urlsplit(path)
        device = parse_qs(request.query).get("device_id", [None])[0]
        conversation = ctl.get("fail_handshake_conversation")
        if conversation and request.path != f"/v1/e2ee/conversations/{conversation}/handshake":
            return False
        if (method != "GET" or not request.path.startswith("/v1/e2ee/conversations/")
                or not request.path.endswith("/handshake") or not target or device != target or target in self.used):
            return False
        self.used.add(target)
        return True


HANDSHAKE_FAULT = HandshakeFault()


def handshake_gate(method, path, ctl):
    """Match only the fixture's two named GET readers; no credential inspection.

    The query device_id is routing scope, not authentication. The real service
    still authenticates the forwarded request. Keep tokens out of control/logs.
    Invalid matching controls fail closed before any HTTP bytes go upstream.
    """
    gate = ctl.get("handshake_gate")
    if not isinstance(gate, dict) or method != "GET":
        return None
    request = urlsplit(path)
    if request.path != f"/v1/e2ee/conversations/{gate.get('conversation_id')}/handshake":
        return None
    devices = gate.get("device_ids")
    query_devices = parse_qs(request.query).get("device_id", [])
    if not isinstance(devices, list) or not any(device in devices for device in query_devices):
        return None
    if len(query_devices) != 1 or len(devices) not in (1, 2) or len(set(devices)) != len(devices):
        raise ValueError("invalid handshake gate device scope")
    for value in [gate.get("id"), gate.get("conversation_id"), *devices]:
        if not isinstance(value, str) or str(uuid.UUID(value)) != value:
            raise ValueError("invalid handshake gate UUID")
    for key in ("max_wait_seconds", "expires_at"):
        value = gate.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise ValueError("invalid handshake gate deadline")
    if not 0 < gate["max_wait_seconds"] <= 120 or gate.get("state") not in ("paused", "released"):
        raise ValueError("invalid handshake gate bounds or state")
    return gate, query_devices[0]


class HandshakeDeliveryGate:
    """At most one pending GET per gate/device; explicit same-scope release only."""
    def __init__(self):
        self.pending = set()
        self.active = None
        self.deadline = None

    async def wait(self, method, path, ctl, cid):
        # Once armed, missing/replaced control cannot make a retry bypass the
        # hold. Retain its two-device scope until the exact explicit release.
        if self.active is not None and ctl.get("handshake_gate") == dict(self.active, state="released"):
            self.active = None
        match = handshake_gate(method, path, {"handshake_gate": self.active}) if self.active else None
        if match is None:
            match = handshake_gate(method, path, ctl)
        if match is None or match[0]["state"] == "released":
            return True
        gate, device = match
        if self.active is None:
            self.active = gate
            self.deadline = time.monotonic() + max(0, min(gate["max_wait_seconds"], gate["expires_at"] - time.time()))
        elif gate != self.active:
            raise ValueError("another handshake gate is already armed")
        key = gate["id"], device
        record = {"conn": cid, "gate_id": gate["id"], "device_id": device,
                  "conversation_id": gate["conversation_id"]}
        if key in self.pending:
            # The app's ordinary 10s HTTP deadline is shorter than this bounded
            # fixture gate. Keep its first slot held, refuse reconnects without
            # forwarding and never extend the shared deadline. This is expected
            # fault setup, not an extra held slot or successful handshake.
            log(dict(record, event="handshake_gate_retry_refused"))
            return False
        self.pending.add(key)
        deadline = self.deadline
        scope = dict(gate, state="released")
        log(dict(record, event="handshake_gate_held"))
        try:
            while time.monotonic() < deadline:
                current = control().get("handshake_gate")
                if current == scope:
                    log(dict(record, event="handshake_gate_released"))
                    return True
                await asyncio.sleep(min(.025, max(0, deadline - time.monotonic())))
            log(dict(record, event="handshake_gate_timeout"))
            return False
        finally:
            self.pending.discard(key)


HANDSHAKE_GATE = HandshakeDeliveryGate()


def control():
    try:
        with open(ARGS.control, "r", encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def log(rec):
    rec["t"] = round(time.time(), 3)
    with open(os.path.join(ARGS.out, "wire.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps(rec) + "\n")


def save_observed():
    path = os.path.join(ARGS.out, "observed-links.json")
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(observed, f, indent=1)
    os.replace(tmp, path)


def b64d(s):
    s = s.strip()
    pad = "=" * (-len(s) % 4)
    try:
        return base64.b64decode(s + pad, altchars=b"-_") if ("-" in s or "_" in s) else base64.b64decode(s + pad)
    except Exception:
        return None


def fresh_pub():
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives import serialization

    k = ec.generate_private_key(ec.SECP256R1())
    return k.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)


async def read_head(reader):
    data = await reader.readuntil(b"\r\n\r\n")
    if len(data) > HEAD_LIMIT:
        raise ValueError("head too large")
    lines = data.decode("latin-1").split("\r\n")
    first = lines[0]
    headers = []
    for line in lines[1:]:
        if not line:
            continue
        k, _, v = line.partition(":")
        headers.append((k.strip(), v.strip()))
    return first, headers


def hget(headers, name):
    name = name.lower()
    for k, v in headers:
        if k.lower() == name:
            return v
    return None


def hset(headers, name, value):
    out = [(k, v) for k, v in headers if k.lower() != name.lower()]
    if value is not None:
        out.append((name, value))
    return out


def head_bytes(first, headers):
    return (first + "\r\n" + "".join(f"{k}: {v}\r\n" for k, v in headers) + "\r\n").encode("latin-1")


class Flipper:
    """Flips the byte at absolute body offset `at` (XOR 0x01) as the body streams through."""

    def __init__(self, at):
        self.at = at
        self.seen = 0
        self.done = at is None

    def __call__(self, chunk):
        if self.done or not chunk:
            self.seen += len(chunk)
            return chunk
        end = self.seen + len(chunk)
        if self.seen <= self.at < end:
            b = bytearray(chunk)
            b[self.at - self.seen] ^= 0x01
            chunk = bytes(b)
            self.done = True
        self.seen = end
        return chunk


async def pipe_body(reader, writer, headers, *, until_close=False, mutate=None):
    """Copies one message body. Returns its length in bytes (decoded, for chunked)."""
    mutate = mutate or (lambda c: c)
    n = 0
    te = (hget(headers, "Transfer-Encoding") or "").lower()
    if "chunked" in te:
        while True:
            line = await reader.readuntil(b"\r\n")
            size = int(line.split(b";")[0].strip(), 16)
            writer.write(line)
            if size == 0:
                # trailers until an empty line
                while True:
                    t = await reader.readuntil(b"\r\n")
                    writer.write(t)
                    if t == b"\r\n":
                        break
                break
            left = size
            while left:
                chunk = await reader.read(min(left, 65536))
                if not chunk:
                    raise ConnectionError("eof in chunk")
                left -= len(chunk)
                n += len(chunk)
                writer.write(mutate(chunk))
                await writer.drain()
            writer.write(await reader.readexactly(2))
        await writer.drain()
        return n
    cl = hget(headers, "Content-Length")
    if cl is not None:
        left = int(cl)
        while left:
            chunk = await reader.read(min(left, 65536))
            if not chunk:
                raise ConnectionError("eof in body")
            left -= len(chunk)
            n += len(chunk)
            writer.write(mutate(chunk))
            await writer.drain()
        return n
    if until_close:
        while True:
            chunk = await reader.read(65536)
            if not chunk:
                break
            n += len(chunk)
            writer.write(mutate(chunk))
            await writer.drain()
    return n


async def splice(r, w):
    try:
        while True:
            chunk = await r.read(65536)
            if not chunk:
                break
            w.write(chunk)
            await w.drain()
    except (ConnectionError, asyncio.IncompleteReadError):
        pass
    finally:
        try:
            w.close()
        except Exception:
            pass


async def connect_upstream(host, port, cid):
    # Windows ConnectEx can transiently reject a fresh loopback endpoint (ERROR_DUP_NAME).
    # No HTTP bytes have been sent at this point, so these bounded connection retries cannot
    # duplicate a message, registration or archive upload. Never retry after forwarding begins.
    for attempt in range(3):
        try:
            return await asyncio.open_connection(host, port)
        except OSError as error:
            if getattr(error, "winerror", None) != 52 or attempt == 2:
                raise
            log({"conn": cid, "upstream_connect_retry": attempt + 1, "winerror": 52})
            await asyncio.sleep(0.05 * (attempt + 1))


async def handle(creader, cwriter):
    cid = next(conn_ids)
    host, port = ARGS.upstream.rsplit(":", 1)
    try:
        ureader, uwriter = await connect_upstream(host, int(port), cid)
    except OSError as e:
        log({"conn": cid, "error": f"upstream: {e}"})
        cwriter.close()
        return
    try:
        while True:
            try:
                first, headers = await read_head(creader)
            except (asyncio.IncompleteReadError, ConnectionError):
                break
            method, path, _ = (first.split(" ", 2) + ["", ""])[:3]
            ctl = control()
            m = LINK_RE.match(path)
            rec = {"conn": cid, "method": method, "path": path.split("?")[0], "query": "?" in path}
            if not await HANDSHAKE_GATE.wait(method, path, ctl, cid):
                body = b'{"error":"proof handshake gate not released"}'
                cwriter.write(head_bytes("HTTP/1.1 503 Service Unavailable", [
                    ("Content-Type", "application/json"), ("Content-Length", str(len(body))),
                    ("Connection", "close")]) + body)
                await cwriter.drain()
                rec.update(status=503, injected="handshake_gate_refused")
                log(rec)
                break
            if HANDSHAKE_FAULT.take(method, path, ctl):
                await pipe_body(creader, _Sink(), headers)
                body = b'{"error":"temporary proof handshake failure"}'
                cwriter.write(head_bytes("HTTP/1.1 503 Service Unavailable", [("Content-Type", "application/json"), ("Content-Length", str(len(body)))]) + body)
                await cwriter.drain()
                rec.update(status=503, injected="handshake_once:" + ctl["fail_handshake_once_for"])
                log(rec)
                continue
            if m:
                rec["link"] = m.group(1)
                rec["route"] = (m.group(2) or "").strip("/") or ("link" if m.group(1) else "links")
                if hget(headers, "X-Tiny-Link-From"):
                    rec["from_header"] = hget(headers, "X-Tiny-Link-From")
            if m and ctl.get("drop_link_routes"):
                await pipe_body(creader, _Sink(), headers)
                body = b'{"error":"not found"}'
                cwriter.write(head_bytes("HTTP/1.1 404 Not Found", [("Content-Type", "application/json"), ("Content-Length", str(len(body)))]) + body)
                await cwriter.drain()
                rec.update(status=404, injected="drop_link_routes")
                log(rec)
                continue

            # A link message: read it whole (small) so it can be looked at and, under
            # swap_old_pub, rewritten. Only the public key is ever recorded.
            if m and rec.get("route") == "messages" and method == "POST":
                cl = int(hget(headers, "Content-Length") or 0)
                body = await creader.readexactly(cl) if cl and cl <= SMALL_BODY else b""
                try:
                    msg = json.loads(body or b"{}")
                except ValueError:
                    msg = {}
                rec["from"] = msg.get("from")
                link = m.group(1)
                entry = observed.setdefault(link, {"messages": 0})
                entry["messages"] += 1
                raw = b64d(msg.get("data", "")) if isinstance(msg.get("data"), str) else None
                if msg.get("from") == "old" and raw is not None and len(raw) == 65 and raw[0] == 4 and "old_pub" not in entry:
                    entry["old_pub"] = base64.urlsafe_b64encode(raw).decode().rstrip("=")
                    if ctl.get("swap_old_pub"):
                        fake = fresh_pub()
                        entry["swapped_to"] = base64.urlsafe_b64encode(fake).decode().rstrip("=")
                        std = "-" not in msg["data"] and "_" not in msg["data"]
                        enc = base64.b64encode(fake).decode() if std else base64.urlsafe_b64encode(fake).decode()
                        if not msg["data"].endswith("="):
                            enc = enc.rstrip("=")
                        msg["data"] = enc
                        body = json.dumps(msg).encode()
                        rec["injected"] = "swap_old_pub"
                save_observed()
                headers = hset(headers, "Content-Length", str(len(body)))
                uwriter.write(head_bytes(first, headers) + body)
                await uwriter.drain()
                rec["req_bytes"] = len(body)
            else:
                uwriter.write(head_bytes(first, headers))
                await uwriter.drain()
                upgrade = (hget(headers, "Upgrade") or "") != ""
                if not upgrade:
                    try:
                        rec["req_bytes"] = await pipe_body(creader, uwriter, headers)
                    except (ConnectionError, OSError):
                        # The server answered before taking the whole body (a 409/413 refusal) and
                        # closed. Its answer is still in the socket: forward it, then end this
                        # connection, since the client's unread body is still on it.
                        rec["upstream_closed_early"] = True

            rfirst, rheaders = await read_head(ureader)
            status = int(rfirst.split(" ")[1])
            rec["status"] = status
            cwriter.write(head_bytes(rfirst, rheaders))
            await cwriter.drain()
            if status == 101:
                rec["upgrade"] = True
                log(rec)
                await asyncio.gather(splice(creader, uwriter), splice(ureader, cwriter))
                return
            mutate = None
            if m and rec.get("route") == "archive" and method == "GET" and status in (200, 206) and ctl.get("tamper_archive"):
                mutate = Flipper(int(ctl.get("tamper_offset", 70000)))
                rec["injected"] = f"tamper_archive@{mutate.at}"
            no_body = method == "HEAD" or status in (204, 304) or 100 <= status < 200
            if not no_body:
                close = (hget(rheaders, "Connection") or "").lower() == "close"
                rec["resp_bytes"] = await pipe_body(ureader, cwriter, rheaders, until_close=close, mutate=mutate)
                if mutate is not None and not mutate.done:
                    rec["injected"] += " (body shorter than offset: NOT tampered)"
            log(rec)
            if (hget(rheaders, "Connection") or "").lower() == "close" or rec.get("upstream_closed_early"):
                break
    except Exception as e:  # a broken connection ends that connection only
        log({"conn": cid, "error": f"{type(e).__name__}: {e}"})
    finally:
        for w in (uwriter, cwriter):
            try:
                w.close()
            except Exception:
                pass


class _Sink:
    def write(self, _):
        pass

    async def drain(self):
        pass


async def main():
    global ARGS
    p = argparse.ArgumentParser()
    p.add_argument("--listen", default="0.0.0.0:8094")
    p.add_argument("--upstream", default="127.0.0.1:8093")
    p.add_argument("--out", required=True)
    p.add_argument("--control", required=True)
    ARGS = p.parse_args()
    os.makedirs(ARGS.out, exist_ok=True)
    host, port = ARGS.listen.rsplit(":", 1)
    server = await asyncio.start_server(handle, host, int(port), limit=HEAD_LIMIT * 2)
    print(f"linkproxy {ARGS.listen} -> {ARGS.upstream}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(0)
