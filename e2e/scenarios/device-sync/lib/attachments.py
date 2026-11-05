"""Check imported attachment keys by fetching and hashing the actual plaintext file."""
import hashlib


def picture_baseline(controls, source, seconds=60):
    """Use two original devices' downloads as the strict reference after upload cleaning."""
    result = dict(source, ok=False, source_sha256=source["sha256"], source_bytes=source["bytes"],
                  reference="matching pre-link original-device downloads", controls=[])
    if len(controls) < 2:
        return dict(result, error="two original-device controls required")
    for device, message in controls:
        observed = verify_picture(device, message, source, seconds)
        observed["matches_source_input"] = observed.pop("ok")
        result["controls"].append(dict(observed, control=getattr(device, "label", "original device")))
    if any("error" in item or not item.get("actual_sha256") or not item.get("actual_bytes")
           for item in result["controls"]):
        return dict(result, error="an original-device attachment did not download")
    fingerprints = {(item["actual_sha256"], item["actual_bytes"]) for item in result["controls"]}
    if len(fingerprints) != 1:
        return dict(result, error="original-device attachment downloads disagree")
    sha, size = fingerprints.pop()
    return dict(result, ok=True, sha256=sha, bytes=size)


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
