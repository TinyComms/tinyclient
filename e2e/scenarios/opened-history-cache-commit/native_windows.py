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


def shot(pid: int, path: str, printwindow: bool = True) -> None:
    action = "printwindow" if printwindow else "capture"
    _ps(["-ExpectedPid", str(pid), "-Action", action, "-Screenshot", path])


def click(pid: int, x: int, y: int) -> None:
    _ps(["-ExpectedPid", str(pid), "-Action", "click", "-X", str(x), "-Y", str(y)])


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

    Returns True/False when Tesseract OCR is available; None when OCR is absent so
    the caller must rely on human/lab review of the screenshot plus `--text` assist.
    """
    try:
        from PIL import Image  # type: ignore
        import pytesseract  # type: ignore
    except Exception:
        return None
    image = Image.open(path)
    text = pytesseract.image_to_string(image)
    return marker in text


def require_windows() -> None:
    if sys.platform != "win32":
        raise NativeError("native_windows driver is Windows-only")
    if not OWNED_INPUT.is_file():
        raise NativeError("owned_input.ps1 missing beside native_windows.py")
