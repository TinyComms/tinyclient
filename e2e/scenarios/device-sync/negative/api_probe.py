"""Server contract probe for device linking: every route and limit in device-link-protocol.md,
against the throwaway server, as Ada (two of her sessions) and Bea. No app involved.

It runs a whole link between two made-up devices (lib/linkcrypto.py): new posts nothing in the
clear, old posts its public key, both seal JSON with the derived keys, old uploads an archive of
canary bytes, new downloads it with and without Range, both delete. Then the limits.

Every check is one line in <out>/api-probe.jsonl and a summary in <out>/api-probe.json.
Exit 0 only when every check held. Against a server without the routes every check fails, which
is the expected result on the base (main) server.

  python negative/api_probe.py --state %LOCALAPPDATA%\\tiny-e2e\\device-link --out <dir> [--quick]

--quick skips the long-poll timing and the expiry wait. The expiry check needs the server up with
a short link TTL (dl-server.ps1 up -LinkTtlSeconds 20); without one it is reported as skipped.
"""

import argparse
import base64
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "lib"))
import linkcrypto as lc  # noqa: E402

RESULTS = []
OUT = None


def check(name, ok, **detail):
    rec = {"check": name, "ok": bool(ok), **detail}
    RESULTS.append(rec)
    with open(os.path.join(OUT, "api-probe.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps(rec) + "\n")
    print(("ok     " if ok else "FAILED ") + name + ("" if ok else "  " + json.dumps(detail)[:300]))
    return ok


class Api:
    def __init__(self, base, token, who):
        self.base, self.token, self.who = base.rstrip("/"), token, who

    def call(self, method, path, body=None, raw=None, headers=None, timeout=40):
        h = {"Authorization": f"Bearer {self.token}"}
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            h["Content-Type"] = "application/json"
        if raw is not None:
            data = raw
            h["Content-Type"] = "application/octet-stream"
            h["Content-Length"] = str(len(raw))
        h.update(headers or {})
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=h)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                payload = r.read()
                return r.status, payload, dict(r.headers)
        except urllib.error.HTTPError as e:
            return e.code, e.read(), dict(e.headers or {})
        except Exception as e:  # connection refused, timeout
            return 0, str(e).encode(), {}

    def json(self, *a, **k):
        st, payload, hd = self.call(*a, **k)
        try:
            return st, json.loads(payload or b"null")
        except ValueError:
            return st, None


def b64(b):
    return base64.b64encode(b).decode()


def main():
    global OUT
    p = argparse.ArgumentParser()
    p.add_argument("--state", default=os.path.join(os.environ.get("LOCALAPPDATA", "/tmp"), "tiny-e2e", "device-link"))
    p.add_argument("--out", required=True)
    p.add_argument("--quick", action="store_true")
    p.add_argument("--canary", default="DLCANARY-API")
    a = p.parse_args()
    OUT = a.out
    os.makedirs(OUT, exist_ok=True)
    open(os.path.join(OUT, "api-probe.jsonl"), "w").close()
    seed = json.load(open(os.path.join(a.state, "seed.json"), encoding="utf-8-sig"))
    server = json.load(open(os.path.join(a.state, "server.json"), encoding="utf-8-sig"))
    base = server["public_url"]
    s = seed["sessions"]
    ada_old = Api(base, s["a_desk_a"]["token"], "ada-old")
    ada_new = Api(base, s["a_spare"]["token"], "ada-new")
    bea = Api(base, s["b_desk"]["token"], "bea")

    st, caps = ada_old.json("GET", "/v1/capabilities")
    check("capabilities advertise e2ee_device_link", st == 200 and isinstance(caps, dict) and caps.get("e2ee_device_link") is True,
          status=st, value=(caps or {}).get("e2ee_device_link") if isinstance(caps, dict) else None)

    # --- a whole link, two made-up devices ---------------------------------------------------
    st, link = ada_new.json("POST", "/v1/e2ee/links", body={})
    ok = check("POST /v1/e2ee/links -> 201 {link_id, expires_at}", st == 201 and isinstance(link, dict) and "link_id" in link and "expires_at" in link, status=st)
    if not ok:
        return finish()
    lid = link["link_id"]
    try:
        lid_raw = lc.unb64u(lid)
    except Exception:
        lid_raw = b""
    check("link_id is 16 random bytes, base64url", len(lid_raw) == 16, link_id_len=len(lid_raw))
    try:
        exp = time.mktime(time.strptime(link["expires_at"][:19], "%Y-%m-%dT%H:%M:%S")) - time.timezone
        ttl = exp - time.time()
    except Exception:
        ttl = None
    check("expires_at about 5 min out (or the configured TTL)", ttl is not None and 0 < ttl <= 330, ttl_seconds=ttl)

    nk, new_pub = lc.new_ephemeral()
    secret = os.urandom(16)
    code = lc.make_code(lid_raw, new_pub, secret)
    with open(os.path.join(OUT, "api-probe-secrets.json"), "w") as f:  # for the canary check: must never reach the server
        json.dump({"code": code, "secret_hex": secret.hex(), "secret_b64u": lc.b64u(secret), "secret_b64": b64(secret)}, f)

    pc = lc.parse_code(code)
    ok_, old_pub = lc.new_ephemeral()
    path = f"/v1/e2ee/links/{lid}/messages"
    st, r = ada_old.json("POST", path, body={"from": "old", "data": b64(old_pub)})
    check("old posts its public key -> 201 {seq}", st == 201 and isinstance(r, dict) and "seq" in r, status=st)
    st, msgs = ada_new.json("GET", path + "?after=0&wait=1")
    got = msgs if isinstance(msgs, list) else []
    check("new reads it: [{seq, from, data}]", st == 200 and any(m.get("from") == "old" and base64.b64decode(m.get("data", "")) == old_pub for m in got), status=st, n=len(got))
    kn = lc.derive(lc.ecdh(nk, old_pub), secret, lid_raw, new_pub, old_pub)
    ko = lc.derive(lc.ecdh(ok_, pc["new_pub"]), pc["secret"], pc["link_id"], pc["new_pub"], old_pub)
    check("both sides derive one check code", kn["check"] == ko["check"], check_code=kn["check"])
    hello = lc.seal(kn, "new_to_old", 0, lid_raw, {"t": "hello", "device_id": "probe-new", "label": a.canary + "-hello", "os": "probe"})
    st, r = ada_new.json("POST", path, body={"from": "new", "data": b64(hello)})
    check("new posts sealed hello -> 201", st == 201, status=st)
    st, msgs = ada_old.json("GET", path + "?after=0&wait=1")
    opened = None
    for m in msgs if isinstance(msgs, list) else []:
        if m.get("from") == "new":
            opened = lc.open_(ko, "new_to_old", 0, lid_raw, base64.b64decode(m["data"]))
    check("old opens hello (the server relayed opaque bytes untouched)", isinstance(opened, dict) and opened.get("t") == "hello")

    # archive: canary plaintext sealed under a fresh key; only ciphertext is uploaded
    akey = os.urandom(32)
    plain = (json.dumps({"r": "msg", "text": a.canary + "-ARCHIVE-PLAINTEXT"}) + "\n").encode() * 3000
    ct = lc.AESGCM(akey).encrypt(b"\x00" * 12, plain, b"tiny-archive-v1" + lid_raw)
    with open(os.path.join(OUT, "api-probe-secrets.json"), "r+") as f:
        sec = json.load(f)
        sec.update(archive_key_hex=akey.hex(), archive_key_b64=b64(akey), archive_key_b64u=lc.b64u(akey))
        f.seek(0)
        json.dump(sec, f)
        f.truncate()
    apath = f"/v1/e2ee/links/{lid}/archive"
    st, _, _ = bea.call("PUT", apath, raw=ct)
    check("Bea cannot upload to Ada's link (404/403)", st in (403, 404), status=st)
    st, _, _ = ada_old.call("PUT", apath, raw=ct)
    check("PUT archive -> 204", st == 204, status=st, bytes=len(ct))
    st, _, _ = ada_old.call("PUT", apath, raw=ct)
    check("PUT archive a second time is refused (once)", st in (409, 400, 403, 404, 410), status=st)
    approve = lc.seal(ko, "old_to_new", 0, lid_raw, {"t": "approve", "history": "all",
                                                     "archive": {"key": b64(akey), "bytes": len(ct), "sha256": b64(hashlib.sha256(ct).digest()), "chunks": 1}})
    st, _ = ada_old.json("POST", path, body={"from": "old", "data": b64(approve)})
    check("old posts sealed approve -> 201", st == 201, status=st)
    st, body, hd = ada_new.call("GET", apath)
    check("new downloads the archive byte for byte", st == 200 and body == ct, status=st, got=len(body or b""), want=len(ct))
    st, body, hd = ada_new.call("GET", apath, headers={"Range": "bytes=100-199"})
    check("archive GET honours Range (206, 100 bytes)", st == 206 and body == ct[100:200], status=st, got=len(body or b""))
    st, _, _ = bea.call("GET", apath)
    check("Bea cannot download Ada's archive", st in (403, 404), status=st)
    st, _ = bea.json("GET", path + "?after=0&wait=0")
    check("Bea cannot read Ada's link messages", st in (403, 404), status=st)
    st, _ = bea.json("POST", path, body={"from": "new", "data": b64(b"x")})
    check("Bea cannot post to Ada's link", st in (403, 404), status=st)
    st, _, _ = bea.call("DELETE", f"/v1/e2ee/links/{lid}")
    st2, _ = ada_new.json("GET", path + "?after=0&wait=0")
    check("Bea's DELETE does not remove Ada's link", st2 == 200, bea_delete=st, ada_after=st2)
    st, _, _ = ada_old.call("DELETE", f"/v1/e2ee/links/{lid}")
    check("DELETE link -> 204", st == 204, status=st)
    st, _, _ = ada_new.call("DELETE", f"/v1/e2ee/links/{lid}")
    check("DELETE again -> 204 (idempotent)", st == 204, status=st)
    st, _ = ada_new.json("GET", path + "?after=0&wait=0")
    check("messages of a deleted link -> 404", st == 404, status=st)
    st, _, _ = ada_new.call("GET", apath)
    check("archive of a deleted link is gone (404)", st in (404, 410), status=st)
    st, _ = ada_old.json("POST", path, body={"from": "old", "data": b64(approve)})
    check("a second approval on a used (deleted) link fails (404)", st == 404, status=st)

    # --- limits --------------------------------------------------------------------------------
    st, l2 = ada_new.json("POST", "/v1/e2ee/links", body={})
    if st == 201:
        lid2 = l2["link_id"]
        mp = f"/v1/e2ee/links/{lid2}/messages"
        st, _ = ada_new.json("POST", mp, body={"from": "sideways", "data": b64(b"x")})
        check("`from` other than new/old is refused (400)", st in (400, 422), status=st)
        st, _ = ada_new.json("POST", mp, body={"from": "new", "data": b64(os.urandom(8 * 1024 + 1))})
        check("data over 8 KiB is refused (413/400)", st in (400, 413, 422), status=st)
        st, _ = ada_new.json("POST", mp, body={"from": "new", "data": b64(os.urandom(8 * 1024))})
        check("data of exactly 8 KiB is accepted", st == 201, status=st)
        codes = []
        for i in range(16):
            st, _ = ada_new.json("POST", mp, body={"from": "new", "data": b64(b"m%d" % i)})
            codes.append(st)
        # 1 accepted above + 15 more = 16; the 17th must fail
        check("at most 16 messages per link", codes[:15].count(201) == 15 and codes[15] in (409, 413, 429, 400, 422), statuses=codes)
        st, _, _ = ada_new.call("PUT", f"/v1/e2ee/links/{lid2}/archive", raw=b"", headers={"Content-Length": "0"})
        # A missing Content-Length cannot be sent through urllib with a body; send chunked by hand.
        import http.client
        from urllib.parse import urlparse
        u = urlparse(base)
        hc = http.client.HTTPConnection(u.hostname, u.port, timeout=20)
        hc.putrequest("PUT", f"/v1/e2ee/links/{lid2}/archive")
        hc.putheader("Authorization", f"Bearer {ada_new.token}")
        hc.putheader("Content-Type", "application/octet-stream")
        hc.putheader("Transfer-Encoding", "chunked")
        hc.endheaders()
        hc.send(b"5\r\nhello\r\n0\r\n\r\n")
        st = hc.getresponse().status
        check("archive PUT without Content-Length is refused (411/400)", st in (400, 411), status=st)
        ada_new.call("DELETE", f"/v1/e2ee/links/{lid2}")

        if not a.quick:
            st, l3 = ada_new.json("POST", "/v1/e2ee/links", body={})
            if st == 201:
                t0 = time.time()
                st, msgs = ada_new.json("GET", f"/v1/e2ee/links/{l3['link_id']}/messages?after=0&wait=25", timeout=40)
                dt = time.time() - t0
                check("long poll waits when empty (about 25 s), answers []", st == 200 and msgs == [] and 20 <= dt <= 32, status=st, seconds=round(dt, 1))
                ada_new.call("DELETE", f"/v1/e2ee/links/{l3['link_id']}")

    made = []
    statuses = []
    for _ in range(6):
        st, l = ada_new.json("POST", "/v1/e2ee/links", body={})
        statuses.append(st)
        if st == 201:
            made.append(l["link_id"])
    check("at most 5 open links per person (6th -> 429)", statuses[:5] == [201] * 5 and statuses[5] == 429, statuses=statuses)
    st, l = bea.json("POST", "/v1/e2ee/links", body={})
    check("the limit is per person: Bea can still open one", st == 201, status=st)
    if st == 201:
        bea.call("DELETE", f"/v1/e2ee/links/{l['link_id']}")
    for lid_ in made:
        ada_new.call("DELETE", f"/v1/e2ee/links/{lid_}")

    # --- expiry ----------------------------------------------------------------------------------
    ttl_cfg = int(server.get("link_ttl_seconds") or 0)
    if ttl_cfg and not a.quick:
        st, l = ada_new.json("POST", "/v1/e2ee/links", body={})
        if st == 201:
            time.sleep(ttl_cfg + 3)
            st, _ = ada_old.json("POST", f"/v1/e2ee/links/{l['link_id']}/messages", body={"from": "old", "data": b64(old_pub)})
            check(f"an expired link (TTL {ttl_cfg}s) answers 404", st == 404, status=st)
    else:
        RESULTS.append({"check": "expired link answers 404", "ok": None, "skipped": "server not started with -LinkTtlSeconds, or --quick"})
        print("skip   expired link answers 404 (no short TTL)")
    return finish()


def finish():
    failed = [r for r in RESULTS if r.get("ok") is False]
    summary = {"checks": len(RESULTS), "failed": len(failed), "passed": sum(1 for r in RESULTS if r.get("ok")),
               "skipped": sum(1 for r in RESULTS if r.get("ok") is None), "results": RESULTS}
    with open(os.path.join(OUT, "api-probe.json"), "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=1)
    print(f"api probe: {summary['passed']} passed, {summary['failed']} failed, {summary['skipped']} skipped")
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
