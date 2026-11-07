"""Search retained throwaway-server files for chosen plaintext and link-secret markers.

Searches, after a run:
  - a pg_dump of the throwaway database (docker exec tiny-dl-pg pg_dump),
  - every file under the server's data dir (logs ring, server key, uploads = the media volume where
    the link archive lives while in flight),
  - the server's stdout/stderr logs,
  - the proxy's wire log (paths and sizes only; it must not hold a code either),
for:
  - the run's canary prefix (every pre-link message text and the picture's drawn/tEXt text carry it),
    as UTF-8, UTF-16LE, base64 and base64url of the text (a server that stored the plaintext
    base64-encoded is still a leak),
  - each link code the new devices showed, and its 16-byte `secret` (hex, base64, base64url):
    the out-of-band authenticator must never reach the server,
  - the api probe's archive key and secret (negative/api_probe.py writes them), when present.

Writes <out>/canary-check.json: {ok, searched: {files, bytes}, hits: [{needle-kind, file, offset}]}.
Never prints a secret, only which kind of needle hit where.

  python negative/canary_check.py --state <dl state dir> --inputs <canary-inputs.json> --out <dir> [--probe <probe dir>]
"""
import argparse
import base64
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "lib"))
import linkcrypto as lc  # noqa: E402


def forms(kind, b):
    """The byte strings a leak of `b` could look like."""
    out = [(kind, b)]
    if len(b) >= 12:
        out.append((kind + ":b64", base64.b64encode(b).rstrip(b"=")[:-2]))
        out.append((kind + ":b64url", base64.urlsafe_b64encode(b).rstrip(b"=")[:-2]))
    try:
        out.append((kind + ":utf16", b.decode("utf-8").encode("utf-16-le")))
    except UnicodeDecodeError:
        pass
    return out


def needles(inputs, probe):
    n = []
    canary = inputs["canary"].encode()
    n += forms("canary-text", canary)
    for code in inputs.get("link_codes", []):
        n += forms("link-code", code.encode())
        if code.startswith(lc.PREFIX):
            try:
                p = lc.parse_code(code)
                n += [("link-secret:hex", p["secret"].hex().encode())] + forms("link-secret", p["secret"])
            except ValueError:
                pass
    if probe and os.path.exists(probe):
        s = json.load(open(probe))
        n += forms("probe-canary", b"DLCANARY-API-ARCHIVE-PLAINTEXT")
        for k in ("secret_hex", "archive_key_hex"):
            if s.get(k):
                raw = bytes.fromhex(s[k])
                n += [(f"probe-{k}:hex", s[k].encode())] + forms(f"probe-{k}", raw)
        n += forms("probe-code", s["code"].encode())
    # drop needles too short to mean anything
    return [(k, v) for k, v in n if len(v) >= 8]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--state", required=True)
    p.add_argument("--inputs", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--probe", default="", help="api-probe-secrets.json of an api_probe run on this server")
    a = p.parse_args()
    inputs = json.load(open(a.inputs))
    server = json.load(open(os.path.join(a.state, "server.json"), encoding="utf-8-sig"))
    dump = os.path.join(a.out, "server-db.sql")
    r = subprocess.run(["docker", "exec", server["pg"], "pg_dump", "-U", "postgres", "--data-only", server["pg_db"]], capture_output=True)
    dump_ok = r.returncode == 0 and bool(r.stdout)
    open(dump, "wb").write(r.stdout)
    roots = [server["data_dir"], server["logs"], server["wire"], dump]
    ns = needles(inputs, a.probe)
    hits, files, total = [], 0, 0
    for root in roots:
        paths = [root] if os.path.isfile(root) else [os.path.join(d, f) for d, _, fs in os.walk(root) for f in fs]
        for path in paths:
            try:
                data = open(path, "rb").read()
            except OSError:
                continue
            files += 1
            total += len(data)
            for kind, v in ns:
                i = data.find(v)
                if i >= 0:
                    hits.append({"needle": kind, "file": os.path.relpath(path, a.state) if path.startswith(a.state) else os.path.basename(path), "offset": i})
    # the canary must also be shown to BE in the app's own store (a positive control), or a check
    # that finds nothing could just be looking in the wrong place: chatd's database of Ada A.
    control = os.path.join(a.state, "devices", "a_desk_a", "chat.sqlite")
    ctrl = None
    if os.path.exists(control):
        blob = b"".join(open(os.path.join(os.path.dirname(control), f), "rb").read() for f in os.listdir(os.path.dirname(control)) if f.startswith("chat.sqlite"))
        ctrl = inputs["canary"].encode() in blob
    # Clean only counts when the canary is shown to exist where it should (the client's store);
    # otherwise the search proves nothing: inconclusive (None).
    res = {"ok": (False if hits or not dump_ok else (True if ctrl else None)), "database_dump_ok": dump_ok,
           "hits": hits, "searched": {"files": files, "bytes": total, "needles": sorted({k for k, _ in ns}), "roots": roots},
           "positive_control_in_client_store": ctrl}
    json.dump(res, open(os.path.join(a.out, "canary-check.json"), "w"), indent=1)
    os.remove(dump) if not hits else None   # keep the dump only when it shows a leak
    outcome = 'chosen markers absent' if res['ok'] is True else (str(len(hits)) + ' HITS' if hits else 'inconclusive')
    print(f"canary check: {outcome} in {files} files / {total} bytes; "
          f"positive control (canary in Ada A's own store): {ctrl}")
    return 0 if res["ok"] is True else 1


if __name__ == "__main__":
    sys.exit(main())
