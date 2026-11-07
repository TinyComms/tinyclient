import hashlib
import os
import tempfile
import unittest

from lib.attachments import verify_picture, picture_baseline


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

    def controls(self, bodies):
        class Device:
            def __init__(self, body):
                self.body = body
            def account(self):
                return {"network": "tiny", "account": "control"}
            def ipc(self, request):
                return 0, {"files": [{"id": "picture", "path": "cache"}]}
            def wait_until(self, name, check, seconds):
                return check()
            def read_file(self, path):
                return self.body
        message = {"attachments": [{"name": "picture.png", "file": {"id": "picture"}}]}
        source = {"name": "picture.png", "sha256": hashlib.sha256(b"raw PNG with metadata").hexdigest(), "bytes": 21}
        return [(Device(body), message) for body in bodies], source

    def test_reference_uses_matching_original_device_downloads_after_metadata_cleaning(self):
        controls, source = self.controls([b"cleaned PNG", b"cleaned PNG"])
        result = picture_baseline(controls, source)
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["sha256"], hashlib.sha256(b"cleaned PNG").hexdigest())
        self.assertEqual(result["bytes"], len(b"cleaned PNG"))
        self.assertEqual(result["source_sha256"], source["sha256"])

    def test_reference_refuses_disagreeing_original_devices(self):
        controls, source = self.controls([b"cleaned PNG", b"different bytes"])
        self.assertFalse(picture_baseline(controls, source)["ok"])

    def test_reference_needs_two_successful_controls(self):
        controls, source = self.controls([b"cleaned PNG"])
        self.assertFalse(picture_baseline(controls, source)["ok"])
        controls, source = self.controls([b"cleaned PNG", b"cleaned PNG"])
        controls[1][1]["attachments"] = []
        self.assertFalse(picture_baseline(controls, source)["ok"])


if __name__ == "__main__":
    unittest.main()
