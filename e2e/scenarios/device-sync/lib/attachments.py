"""Check imported attachment keys by fetching and hashing the actual plaintext file."""
import hashlib


def verify_picture(device, message, expected, seconds=60):
    attachment = next((a for a in message.get("attachments", []) if a.get("name") == expected["name"]), {})
    file_id = attachment.get("file", {}).get("id")
    result = {"ok": False, "name": expected["name"], "file_id": file_id,
              "expected_sha256": expected["sha256"], "expected_bytes": expected["bytes"]}
    if not file_id:
        return dict(result, error="attachment reference missing")

    def fetch():
        rc, answer = device.ipc({"op": "fetch", "account": device.account(), "ids": [file_id]})
        if rc != 0 or not isinstance(answer, dict):
            return None
        return next((f for f in answer.get("files", []) if f.get("id") == file_id and f.get("path")), None)

    fetched = device.wait_until("picture fetched", fetch, seconds)
    if not fetched:
        return dict(result, error="attachment did not download")
    try:
        data = device.read_file(fetched["path"])
        actual = hashlib.sha256(data).hexdigest()
        return dict(result, actual_sha256=actual, actual_bytes=len(data),
                    ok=actual == expected["sha256"] and len(data) == expected["bytes"])
    except Exception as error:
        return dict(result, error=str(error))
