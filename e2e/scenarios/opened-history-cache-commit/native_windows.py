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


def sidebar_word_center(words, width, name):
    candidates = [word for word in words
                  if word.get("text", "").casefold() == name.casefold()
                  and 0 <= word.get("x", -1) < min(300, width * 0.3)
                  and word.get("y", 0) >= 50
                  and word.get("width", 0) > 0 and word.get("height", 0) > 0]
    if len(candidates) != 1:
        raise NativeError("no unique visible peer name in the sidebar; no click sent")
    word = candidates[0]
    return (round(word["x"] + word["width"] / 2), round(word["y"] + word["height"] / 2))


def windows_ocr(path):
    done = subprocess.run([
        "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", str(HERE / "ocr.windows.ps1"), "-Path", str(Path(path).resolve()),
    ], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=20)
    if done.returncode != 0:
        raise NativeError("Windows pixel text observation unavailable")
    return json.loads(done.stdout)


def select_sidebar_peer(pid, name, screenshot, seconds=15):
    """One click, located from observed text rather than configured coordinates."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        prepare(pid)
        shot(pid, screenshot)
        observed = windows_ocr(screenshot)
        try:
            x, y = sidebar_word_center(observed.get("words", []), rect(pid)["width"], name)
        except NativeError:
            time.sleep(0.5)
            continue
        click(pid, x, y)  # Foreground and hit-PID checked by owned_input.ps1.
        return {"x": x, "y": y, "name": name}
    raise NativeError("peer name never became uniquely visible; no click sent")


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


def require_windows() -> None:
    if sys.platform != "win32":
        raise NativeError("native_windows driver is Windows-only")
    if not OWNED_INPUT.is_file():
        raise NativeError("owned_input.ps1 missing beside native_windows.py")
