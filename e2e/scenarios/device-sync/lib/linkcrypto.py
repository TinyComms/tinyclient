"""Reference implementation of the device-link channel from research_notes/device-link-protocol.md.

The harness uses it for three things and never to stand in for an app in the proof itself:
  1. The server contract probe (negative/api_probe.py) runs a whole link between two made-up
     devices over the real server, so the server routes are proven apart from either client.
  2. The expected check code: from the link code the NEW device shows and the old device's public
     key the proxy saw on the wire, compute the 6 digits both screens must show.
  3. A known-answer test of the contract itself (python -I linkcrypto.py selftest).

Details the contract leaves open are taken from the client branch (crates/e2ee/src/link.rs,
feat/device-link 3d6b1533): the check is u32 big-endian of the 3 bytes mod 10^6; `dir` in the AAD is
one byte, 0 for new->old and 1 for old->new; a sealed message on the wire is counter(8, BE) || ct.

Needs the `cryptography` package (already installed on the coordinator; nothing new is installed).
"""

import base64
import hashlib
import json
import os
import sys

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

PREFIX = "tiny-link-1."
INFO = b"tiny-link-v1"
CHECK_ENDIAN = "big"
DIR_BYTES = {"new_to_old": b"\x00", "old_to_new": b"\x01"}


def b64u(b):
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def unb64u(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def unb64any(s):
    s = s.strip()
    if "-" in s or "_" in s:
        return unb64u(s)
    return base64.b64decode(s + "=" * (-len(s) % 4))


def new_ephemeral():
    k = ec.generate_private_key(ec.SECP256R1())
    pub = k.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
    return k, pub


def make_code(link_id, new_pub, secret):
    assert len(link_id) == 16 and len(new_pub) == 65 and len(secret) == 16
    return PREFIX + b64u(link_id + new_pub + secret)


def parse_code(code):
    code = code.strip()
    if not code.startswith(PREFIX):
        raise ValueError("not a tiny-link-1 code")
    raw = unb64u(code[len(PREFIX):])
    if len(raw) != 97:
        raise ValueError(f"link code payload is {len(raw)} bytes, want 97")
    link_id, new_pub, secret = raw[:16], raw[16:81], raw[81:]
    if new_pub[0] != 4:
        raise ValueError("new_pub is not an uncompressed P-256 point")
    return {"link_id": link_id, "link_id_b64u": b64u(link_id), "new_pub": new_pub, "secret": secret}


def ecdh(priv, peer_pub):
    peer = ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), peer_pub)
    return priv.exchange(ec.ECDH(), peer)


def derive(z, secret, link_id, new_pub, old_pub):
    okm = HKDF(algorithm=hashes.SHA256(), length=67, salt=secret, info=INFO + link_id + new_pub + old_pub).derive(z)
    n = int.from_bytes(okm[64:67], CHECK_ENDIAN) % 10**6
    return {"k_new_to_old": okm[:32], "k_old_to_new": okm[32:64], "check": f"{n:06d}"}


def nonce(counter):
    return b"\x00" * 4 + counter.to_bytes(8, "big")


def aad(link_id, direction, counter):
    return link_id + DIR_BYTES[direction] + counter.to_bytes(8, "big")


def seal(keys, direction, counter, link_id, obj):
    key = keys["k_new_to_old"] if direction == "new_to_old" else keys["k_old_to_new"]
    return counter.to_bytes(8, "big") + AESGCM(key).encrypt(nonce(counter), json.dumps(obj).encode(), aad(link_id, direction, counter))


def open_(keys, direction, counter, link_id, data):
    """`counter` is what the reader expects at least; the message carries its own."""
    key = keys["k_new_to_old"] if direction == "new_to_old" else keys["k_old_to_new"]
    got = int.from_bytes(data[:8], "big")
    if got < counter:
        raise ValueError("counter went back")
    return json.loads(AESGCM(key).decrypt(nonce(got), data[8:], aad(link_id, direction, got)))


def expected_check(code, old_pub):
    """What both screens must show, from the code (new device) and the old key seen on the wire.
    Needs no private key: it cannot be computed without z, so this returns None, and the proof
    instead checks that the two devices agree with EACH OTHER and that the swapped key changes it.
    Kept as a function so the intent is explicit."""
    return None


def selftest():
    link_id, secret = os.urandom(16), os.urandom(16)
    nk, new_pub = new_ephemeral()
    ok, old_pub = new_ephemeral()
    code = make_code(link_id, new_pub, secret)
    p = parse_code(code)
    assert p["link_id"] == link_id and p["new_pub"] == new_pub and p["secret"] == secret
    kn = derive(ecdh(nk, old_pub), secret, link_id, new_pub, old_pub)
    ko = derive(ecdh(ok, p["new_pub"]), p["secret"], p["link_id"], p["new_pub"], old_pub)
    assert kn == ko, "both sides derive the same keys and check"
    ct = seal(kn, "new_to_old", 0, link_id, {"t": "hello"})
    assert open_(ko, "new_to_old", 0, link_id, ct) == {"t": "hello"}
    # A server in the middle that swaps old_pub: the new side's check differs from the old side's.
    _, mitm_pub = new_ephemeral()
    km = derive(ecdh(nk, mitm_pub), secret, link_id, new_pub, mitm_pub)
    assert km["check"] != ko["check"] or km["k_new_to_old"] != ko["k_new_to_old"]
    try:
        open_(ko, "new_to_old", 0, link_id, seal(km, "new_to_old", 0, link_id, {"t": "hello"}))
        raise AssertionError("old device opened a message sealed under the swapped key")
    except Exception as e:
        if isinstance(e, AssertionError):
            raise
    # A replayed (old) counter does not open.
    try:
        open_(ko, "new_to_old", 1, link_id, ct)
        raise AssertionError("replayed counter opened")
    except Exception as e:
        if isinstance(e, AssertionError):
            raise
    print(json.dumps({"selftest": "ok", "code_len": len(code), "check": kn["check"]}))


if __name__ == "__main__":
    if sys.argv[1:2] == ["selftest"]:
        selftest()
    elif sys.argv[1:2] == ["parse"]:
        p = parse_code(sys.argv[2])
        print(json.dumps({"link_id": p["link_id_b64u"], "new_pub": b64u(p["new_pub"]), "secret_hex": p["secret"].hex()}))
    else:
        print("usage: linkcrypto.py selftest | parse <code>")
