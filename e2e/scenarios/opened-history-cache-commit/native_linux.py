"""Owned X11 input and image-only observations for the Linux history driver."""

import csv
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


class NativeError(RuntimeError):
    pass


def _run(args, timeout=15):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise NativeError("native command failed: " + result.stderr[-400:])
    return result.stdout.strip()


def require_linux():
    if not sys.platform.startswith("linux") or not os.environ.get("DISPLAY"):
        raise NativeError("Linux X11 DISPLAY is required")
    if os.environ.get("XDG_SESSION_TYPE") == "wayland":
        raise NativeError("this adapter requires an X11 session")
    for tool in ("wmctrl", "xdotool", "ffmpeg", "tesseract"):
        if not shutil.which(tool):
            raise NativeError("native test dependency is missing: " + tool)


def _owned(pid):
    app = os.environ.get("E2E_APP_DIR", "")
    if not app or pid <= 0:
        raise NativeError("an owned app directory and PID are required")
    try:
        expected = (Path(app) / "tinyclient").resolve(strict=True)
        actual = (Path("/proc") / str(pid) / "exe").resolve(strict=True)
    except OSError as error:
        raise NativeError("owned GUI process is unavailable") from error
    if actual != expected:
        raise NativeError("GUI PID executable differs from the owned app")


def rect(pid):
    _owned(pid)
    candidates = []
    for line in _run(["wmctrl", "-lpG"]).splitlines():
        fields = line.split(None, 8)
        if len(fields) < 8 or not fields[2].isdigit() or int(fields[2]) != pid:
            continue
        try:
            x, y, width, height = map(int, fields[3:7])
            if width > 0 and height > 0:
                candidates.append({"window_id": int(fields[0], 16), "left": x, "top": y,
                                   "width": width, "height": height})
        except ValueError:
            continue
    if not candidates:
        raise NativeError("owned GUI window is unavailable")
    return max(candidates, key=lambda item: item["width"] * item["height"])


def wait_window(pid, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            rect(pid)
            return True
        except NativeError:
            time.sleep(.25)
    return False


def _front(pid):
    window = rect(pid)["window_id"]
    _run(["wmctrl", "-ia", hex(window)])
    time.sleep(.1)
    _focused(pid, window)
    return window


def _focused(pid, window):
    _owned(pid)
    active = int(_run(["xdotool", "getactivewindow"]))
    owner = int(_run(["xdotool", "getwindowpid", str(active)]))
    if active != window or owner != pid:
        raise NativeError("foreground window is not the owned GUI; input refused")


def _key(pid, window, key):
    _focused(pid, window)
    _run(["xdotool", "key", "--clearmodifiers", key])
    _focused(pid, window)


def shot(pid, path, printwindow=False):
    window = _front(pid)
    _run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "x11grab",
          "-window_id", str(window), "-i", os.environ["DISPLAY"], "-frames:v", "1", str(path)])
    _owned(pid)


def ocr_words(path):
    # Small native glyphs can lose a hyphen at 1x. Enlarge the actual captured
    # image rather than relaxing the exact marker match.
    with tempfile.TemporaryDirectory(prefix="tiny-history-ocr-") as folder:
        enlarged = Path(folder) / "pixels.png"
        _run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i",
              str(Path(path).resolve()), "-vf", "scale=iw*2:ih*2:flags=lanczos", str(enlarged)])
        output = _run(["tesseract", str(enlarged), "stdout", "--psm", "11", "tsv"], timeout=30)
    words = []
    for row in csv.DictReader(io.StringIO(output), delimiter="\t", quoting=csv.QUOTE_NONE):
        if row.get("level") != "5" or not row.get("text", "").strip():
            continue
        try:
            words.append({"text": row["text"], "x": int(row["left"]) // 2, "y": int(row["top"]) // 2,
                          "width": int(row["width"]) // 2, "height": int(row["height"]) // 2})
        except (ValueError, KeyError):
            raise NativeError("OCR word geometry is malformed")
    return words


def conversation_marker(words, marker):
    anchors = []
    for word in words:
        if word["text"].casefold() != "this" or word["x"] < 300 or word["y"] < 50:
            continue
        nearby = [item for item in words if item["x"] >= word["x"]
                  and word["y"] - 2 <= item["y"] <= word["y"] + 2 * word["height"] + 4]
        phrase = "".join(item["text"] for item in nearby).casefold()
        if phrase.startswith("thisisthestartofyourconversation"):
            anchors.append((word, max(item["y"] + item["height"] for item in nearby)))
    if not anchors:
        return None
    anchor, bottom = min(anchors, key=lambda item: (item[0]["x"], item[0]["y"]))
    text = "".join(word["text"] for word in words
                   if word["x"] >= anchor["x"] and word["y"] > bottom)
    return marker.casefold() in "".join(text.split()).casefold()


def conversation_mentions_marker(path, marker):
    try:
        return conversation_marker(ocr_words(path), marker)
    except (NativeError, OSError, ValueError, subprocess.TimeoutExpired):
        return None


def select_sidebar_peer(pid, name, screenshot):
    window = _front(pid)
    _key(pid, window, "ctrl+k")
    time.sleep(.4)
    _focused(pid, window)
    _run(["xdotool", "type", "--clearmodifiers", "--delay", "40", "--", name])
    _focused(pid, window)
    time.sleep(.5)
    shot(pid, screenshot)
    text = " ".join(word["text"] for word in ocr_words(screenshot))
    if name.casefold() not in text.casefold():
        raise NativeError("peer query is not visible; Enter refused")
    _key(pid, window, "Return")
    return {"method": "ctrl-k query enter", "name": name, "owned_window": window}
