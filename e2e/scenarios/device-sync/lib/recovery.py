"""Public wire observations; these do not establish device trust or MLS acceptance.

The observer verifies format, signature and pinned external-sender consistency.
Only the actual engine proves independent directory trust, old authenticated
roster membership, successor acceptance and usable new private group state.
Never export device snapshots or private key material into this module's output.
"""
import base64
import hashlib
import uuid

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature


FIELDS = {"version", "conversation_id", "from_generation", "to_generation",
          "from_group_id", "group_id", "from_epoch", "previous_hash",
          "server_key_hash", "user_id", "device_id", "signature_key", "signature"}


def _uuid(value):
    if not isinstance(value, str):
        raise ValueError("binding UUID must be a canonical string")
    parsed = uuid.UUID(value)
    if str(parsed) != value:
        raise ValueError("binding UUID must be canonical")
    return parsed.bytes


def _integer(value):
    if type(value) is not int or not 0 <= value <= 2**63 - 1:
        raise ValueError("binding integer out of range")
    return value.to_bytes(8, "big")


def _base64(value, size=None):
    if not isinstance(value, str):
        raise ValueError("binding base64 must be a string")
    try:
        decoded = base64.b64decode(value, validate=True)
    except (ValueError, TypeError) as error:
        raise ValueError("invalid binding base64") from error
    if base64.b64encode(decoded).decode() != value or (size is not None and len(decoded) != size):
        raise ValueError("noncanonical binding base64 or wrong length")
    return decoded


def _public_key(value):
    raw = _base64(value, 65)
    if raw[0] != 4:
        raise ValueError("binding key must be uncompressed P-256")
    return ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), raw)


def canonical_payload(binding):
    if not isinstance(binding, dict) or set(binding) != FIELDS:
        raise ValueError("binding fields differ from public wire contract")
    if type(binding["version"]) is not int or binding["version"] != 1:
        raise ValueError("unsupported binding version")
    numbers = [_integer(binding[key]) for key in ("from_generation", "to_generation", "from_epoch")]
    if binding["to_generation"] != binding["from_generation"] + 1:
        raise ValueError("binding must advance exactly one generation")
    if binding["group_id"] == binding["from_group_id"]:
        raise ValueError("binding must use a fresh group UUID")
    signature = _base64(binding["signature"])
    if not 8 <= len(signature) <= 72:
        raise ValueError("binding signature length invalid")
    decode_dss_signature(signature)
    _public_key(binding["signature_key"])
    return (b"tiny-e2ee-v1 group-repair\0\x01" + _uuid(binding["conversation_id"])
            + numbers[0] + numbers[1] + _uuid(binding["from_group_id"])
            + _uuid(binding["group_id"]) + numbers[2]
            + _base64(binding["previous_hash"], 32) + _base64(binding["server_key_hash"], 32)
            + _uuid(binding["user_id"]) + _uuid(binding["device_id"])
            + _base64(binding["signature_key"], 65))


def inspect_binding(binding, external_server_key):
    payload = canonical_payload(binding)
    _public_key(external_server_key)
    server_hash = hashlib.sha256(_base64(external_server_key, 65)).digest()
    if server_hash != _base64(binding["server_key_hash"], 32):
        raise ValueError("binding changed the independently pinned server key")
    if binding["from_generation"] == 0:
        genesis = hashlib.sha256(b"tiny-e2ee-v1 group-genesis\0"
                                 + _uuid(binding["conversation_id"]) + server_hash).digest()
        if (binding["from_group_id"] != binding["conversation_id"]
                or _base64(binding["previous_hash"], 32) != genesis):
            raise ValueError("binding does not extend the genesis group")
    try:
        _public_key(binding["signature_key"]).verify(
            _base64(binding["signature"]), payload, ec.ECDSA(hashes.SHA256()))
    except InvalidSignature as error:
        raise ValueError("binding signature invalid") from error
    return {"generation": binding["to_generation"], "group_id": binding["group_id"],
            "head_hash": base64.b64encode(hashlib.sha256(payload).digest()).decode()}


def inspect_repair_response(response, external_server_key):
    if not isinstance(response, dict) or set(response) != {"generation", "group_id", "head_hash", "binding"}:
        raise ValueError("repair response fields differ from public wire contract")
    head = inspect_binding(response["binding"], external_server_key)
    _integer(response["generation"])
    if any(response[key] != value for key, value in head.items()):
        raise ValueError("repair response head differs from signed binding")
    return head
