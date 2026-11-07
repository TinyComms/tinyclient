"""Wire observer checks use a published vector, never the observer's own output."""
import base64
import copy
import json
from pathlib import Path
import unittest

from lib import recovery


VECTOR = json.loads((Path(__file__).parent / "fixtures/repair-binding-v1.json").read_text())


class PublicRepairBindingTest(unittest.TestCase):
    def binding(self):
        return copy.deepcopy(VECTOR["binding"])

    def test_canonical_bytes_match_independent_engine_vector(self):
        self.assertEqual(recovery.canonical_payload(self.binding()).hex(), VECTOR["payload_hex"])

    def test_public_signature_and_hash_match_independent_vector(self):
        self.assertEqual(recovery.inspect_binding(self.binding(), VECTOR["external_server_key"]),
                         {"generation": 1, "group_id": "20000000-0000-4000-8000-000000000002",
                          "head_hash": "1xUPbj5Q21BNhLSYiplWN8wqu2h6D7m5TItGT6lMGFE="})

    def test_altered_conversation_epoch_hash_and_key_cannot_preserve_signature(self):
        for field, value in [("conversation_id", "10000000-0000-4000-8000-000000000009"),
                             ("from_epoch", 8), ("previous_hash", base64.b64encode(b"x" * 32).decode()),
                             ("signature_key", VECTOR["external_server_key"])]:
            with self.subTest(field=field):
                binding = self.binding()
                binding[field] = value
                with self.assertRaises(ValueError):
                    recovery.inspect_binding(binding, VECTOR["external_server_key"])

    def test_external_sender_key_is_checked_independently_of_binding(self):
        with self.assertRaises(ValueError):
            recovery.inspect_binding(self.binding(), self.binding()["signature_key"])

    def test_noncanonical_or_out_of_range_wire_values_are_rejected(self):
        invalid = [("from_epoch", True), ("from_epoch", -1), ("from_epoch", 2**63),
                   ("to_generation", 2), ("version", True),
                   ("group_id", self.binding()["from_group_id"]),
                   ("conversation_id", "10000000000040008000000000000001"),
                   ("previous_hash", self.binding()["previous_hash"].rstrip("=")),
                   ("signature", base64.b64encode(b"x" * 100).decode())]
        for field, value in invalid:
            with self.subTest(field=field, value=value):
                binding = self.binding()
                binding[field] = value
                with self.assertRaises(ValueError):
                    recovery.canonical_payload(binding)

    def test_unknown_fields_never_enter_a_public_binding_record(self):
        binding = self.binding()
        binding["private_key"] = "canary-private-value"
        with self.assertRaises(ValueError):
            recovery.inspect_binding(binding, VECTOR["external_server_key"])

    def test_response_head_must_match_its_exact_signed_binding(self):
        response = {"generation": 1, "group_id": self.binding()["group_id"],
                    "head_hash": VECTOR["head_hash"], "binding": self.binding()}
        self.assertEqual(recovery.inspect_repair_response(response, VECTOR["external_server_key"]),
                         {key: response[key] for key in ("generation", "group_id", "head_hash")})
        for field, value in [("generation", 0), ("group_id", self.binding()["from_group_id"]),
                             ("head_hash", self.binding()["previous_hash"])]:
            with self.subTest(field=field):
                altered = dict(response, **{field: value})
                with self.assertRaises(ValueError):
                    recovery.inspect_repair_response(altered, VECTOR["external_server_key"])
