import hashlib
import os
import tempfile
import unittest

from lib.attachments import verify_picture


class AttachmentFetchTest(unittest.TestCase):
    def verify(self, downloaded):
        original = b"original encrypted picture plaintext"
        class Device:
            def account(self):
                return {"network": "tiny", "account": "ada"}
            def ipc(self, request):
                self.request = request
                return 0, {"op": "media", "files": [{"id": "picture", "path": "device-cache"}]}
            def wait_until(self, name, check, seconds):
                return check()
            def read_file(self, path):
                assert path == "device-cache"
                return downloaded
        device = Device()
        message = {"attachments": [{"name": "picture.png", "file": {"id": "picture"}}]}
        expected = {"name": "picture.png", "sha256": hashlib.sha256(original).hexdigest(), "bytes": len(original)}
        return verify_picture(device, message, expected), device

    def test_fetches_actual_attachment_and_hashes_downloaded_bytes(self):
        result, device = self.verify(b"original encrypted picture plaintext")
        self.assertTrue(result["ok"], result)
        self.assertEqual(device.request, {"op": "fetch", "account": device.account(), "ids": ["picture"]})

    def test_rejects_corrupt_download_even_when_attachment_name_matches(self):
        result, _ = self.verify(b"corrupt encrypted picture plaintext!")
        self.assertFalse(result["ok"])
        self.assertNotEqual(result["actual_sha256"], result["expected_sha256"])


if __name__ == "__main__":
    unittest.main()
