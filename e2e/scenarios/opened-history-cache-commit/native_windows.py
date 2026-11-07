"""Person-facing Windows driver actions for opened-history-cache-commit.

Uses owned_input.ps1 with foreground/hit-PID guards. Does not call chatd prove
modes and does not treat IPC Send as native evidence.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path


HERE = Path(__file__).resolve().parent
OWNED_INPUT = HERE / "owned_input.ps1"


class NativeError(RuntimeError):
    pass


def _ps(args: list[str], timeout: int = 60) -> str:
    cmd = [
        "powershell.exe",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        str(OWNED_INPUT),
        *args,
    ]
    done = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if done.returncode != 0:
        raise NativeError(
            (done.stdout or "")[-400:] + "\n" + (done.stderr or "")[-400:]
        )
    return (done.stdout or "").strip()


def prepare(pid: int, other_pid: int = 0) -> None:
    args = ["-ExpectedPid", str(pid), "-Action", "prepare"]
    if other_pid:
        args += ["-OtherPid", str(other_pid)]
    _ps(args)


def front(pid: int) -> None:
    _ps(["-ExpectedPid", str(pid), "-Action", "front"])


def shot(pid: int, path: str, printwindow: bool = False) -> None:
    action = "printwindow" if printwindow else "capture"
    _ps(["-ExpectedPid", str(pid), "-Action", action, "-Screenshot", path])


def click(pid: int, x: int, y: int) -> None:
    _ps(["-ExpectedPid", str(pid), "-Action", "click", "-X", str(x), "-Y", str(y)])


def windows_ocr(path):
    done = subprocess.run([
        "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", str(HERE / "ocr.windows.ps1"), "-Path", str(Path(path).resolve()),
    ], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=20)
    if done.returncode != 0:
        raise NativeError("Windows pixel text observation unavailable")
    return json.loads(done.stdout)


def select_sidebar_peer(pid, name, screenshot):
    """Use the app's normal Jump command, with query pixels checked before Enter."""
    prepare(pid)
    _ps(["-ExpectedPid", str(pid), "-Action", "type", "-Text", "^k"])
    time.sleep(0.4)
    # SendKeys treats these characters as commands unless escaped.
    literal = "".join("{{}" if ch == "{" else "{}}" if ch == "}" else
                      "{" + ch + "}" if ch in "+^%~()[]" else ch for ch in name)
    _ps(["-ExpectedPid", str(pid), "-Action", "type", "-Text", literal])
    time.sleep(0.7)
    shot(pid, screenshot)
    if name.casefold() not in windows_ocr(screenshot)["text"].casefold():
        raise NativeError("peer query is not visible in Jump; refused Enter")
    _ps(["-ExpectedPid", str(pid), "-Action", "type", "-Text", "{ENTER}"])
    return {"method": "ctrl-k query enter", "name": name}


def rect(pid: int) -> dict:
    raw = _ps(["-ExpectedPid", str(pid), "-Action", "rect"])
    return json.loads(raw)


def wait_window(pid: int, seconds: float = 45.0) -> bool:
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            rect(pid)
            return True
        except NativeError:
            time.sleep(0.4)
    return False


def png_mentions_marker(path: str, marker: str) -> bool | None:
    """Best-effort pixel/text gate.

    Uses Tesseract when installed, otherwise the Windows OCR API. Missing/broken OCR returns
    None, which must make the run incomplete rather than certifying IPC as pixels.
    """
    try:
        from PIL import Image  # type: ignore
        import pytesseract  # type: ignore
        image = Image.open(path)
        text = pytesseract.image_to_string(image)
    except Exception:
        try:
            if sys.platform != "win32":
                return None
            text = windows_ocr(path)["text"]
        except Exception:
            return None
    return marker in "".join(text.split())


def conversation_mentions_marker(path, marker):
    try:
        words = windows_ocr(path)["words"]
        anchors = [word for word in words if word.get("text", "").casefold() == "this"
                   and word.get("x", 0) >= 300 and word.get("y", 0) >= 50]
        if not anchors:
            return None  # No fresh-conversation intro; a sidebar preview cannot certify the pane.
        anchor = min(anchors, key=lambda word: word["x"])
        text = "".join(word["text"] for word in words
                       if word["x"] >= anchor["x"] and word["y"] > anchor["y"] + anchor["height"])
        return marker in "".join(text.split())
    except Exception:
        return None


def require_windows() -> None:
    if sys.platform != "win32":
        raise NativeError("native_windows driver is Windows-only")
    if not OWNED_INPUT.is_file():
        raise NativeError("owned_input.ps1 missing beside native_windows.py")
