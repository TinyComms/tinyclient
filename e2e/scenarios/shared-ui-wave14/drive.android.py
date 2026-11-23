"""Drive a demo phone using an Android media share and native UI input."""

import base64
import json
import os
import re
import subprocess
import time
import xml.etree.ElementTree as ET
from pathlib import Path

OUT = Path(os.environ["E2E_OUT"])
T0 = float(os.environ["E2E_T0"])
PACKAGE = os.environ.get("E2E_PACKAGE", "dev.tinyclient.app")
DEVICE = [os.environ["E2E_ADB"], "-s", os.environ.get("ANDROID_SERIAL", "emulator-5554")]
PHOTO = "wave14-share.png"
PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j18sAAAAASUVORK5CYII="


def adb(*args):
    return subprocess.check_output(DEVICE + list(args), timeout=30).decode().strip()


def step(words, ok):
    with (OUT / "steps.jsonl").open("a", encoding="utf-8") as handle:
        print(json.dumps({"t": round(time.time() - T0, 2), "step": words, "ok": ok}), file=handle)


def find(pattern, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            adb("shell", "uiautomator", "dump", "/sdcard/wave14.xml")
            tree = ET.fromstring(adb("shell", "cat", "/sdcard/wave14.xml"))
        except (subprocess.CalledProcessError, ET.ParseError):
            time.sleep(0.25)
            continue
        for node in tree.iter("node"):
            words = [node.get(name, "") for name in ("text", "content-desc")]
            if any(re.search(pattern, word, re.IGNORECASE) for word in words):
                bounds = [int(value) for value in re.findall(r"\d+", node.get("bounds", ""))]
                if len(bounds) == 4 and bounds[0] < bounds[2] and bounds[1] < bounds[3]:
                    return (bounds[0] + bounds[2]) // 2, (bounds[1] + bounds[3]) // 2
    raise RuntimeError("UI does not expose " + pattern)


def tap(pattern):
    x, y = find(pattern)
    adb("shell", "input", "tap", str(x), str(y))


def media_uri():
    photo = OUT / PHOTO
    photo.write_bytes(base64.b64decode(PNG))
    location = "/sdcard/Pictures/" + PHOTO
    adb("push", str(photo), location)
    adb("shell", "am", "broadcast", "-a", "android.intent.action.MEDIA_SCANNER_SCAN_FILE", "-d", "file://" + location)
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        rows = adb("shell", "content", "query", "--uri", "content://media/external/images/media", "--projection", "_id:_display_name")
        match = re.search(r"_id=(\d+), _display_name=" + re.escape(PHOTO), rows)
        if match:
            return "content://media/external/images/media/" + match[1]
        time.sleep(0.2)
    raise RuntimeError("The synthetic photo was never indexed by MediaStore")


def main():
    current = "share a gallery photo and open Tiny's conversation picker"
    try:
        uri = media_uri()
        adb("shell", "am", "start", "-W", "-a", "android.intent.action.SEND", "-t", "image/png",
            "-p", PACKAGE, "--eu", "android.intent.extra.STREAM", uri, "--grant-read-uri-permission")
        find("Share to conversation")
        step(current, True)
        current = "select Nadia and confirm the photo is staged in her draft"
        tap("Nadia Sokolova")
        find(re.escape(PHOTO))
        step(current, True)
        current = "open the attachment menu and choose the system photo picker"
        tap("Attach files")
        tap("Photos and videos")
        find("Photos|Albums|No photos")
        adb("shell", "input", "keyevent", "KEYCODE_BACK")
        step(current, True)
        current = "translate and restore the original message on the phone"
        x, y = find("horario del ferry")
        adb("shell", "input", "swipe", str(x), str(y), str(x), str(y), "700")
        tap(r"^\s*Translate\s*$")
        find("The ferry schedule changed")
        find("Translated from Spanish")
        tap("Show original")
        find("horario del ferry")
        try:
            find("The ferry schedule changed", timeout=3)
        except RuntimeError:
            step(current, True)
            return 0
        raise RuntimeError("Translation is still visible")
    except Exception as error:
        print(error)
        step(current, False)
        return 1
    finally:
        adb("shell", "am", "force-stop", PACKAGE)


if __name__ == "__main__":
    raise SystemExit(main())
