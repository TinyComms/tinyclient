#!/usr/bin/env python3
"""A Telegram photo shows the preview that came inside its message while its thumbnail downloads.

The fake Telegram wire posts a photo (`photo-preview`) whose stripped thumbnail is a 40 by 30
picture: orange on the left, blue on the right. Its chat-sized thumbnail is the same two halves
under fine dark stripes, which the stripped one is too small to keep. The wire holds that
thumbnail's download (TINYCLIENT_TELEGRAM_FILE_HOLD) until the driver lets it go (`file-go`).

1. chatd signs in to the invented account, and the photo is posted to Odalys's chat.
2. The real window opens that chat. Telegram is asked for the thumbnail and holds it.
3. Screen pixels: the photo's place shows orange beside blue with no stripes, the preview.
4. The thumbnail is let go. Screen pixels: the stripes appear over the same place, at the same size.

A build from before the change draws the image icon at step 3. Its fake wire does not know the
photo either, so it fails at step 2.

Env from the lab: E2E_OUT, E2E_APP_DIR, E2E_T0. Linux grabs the X display with ffmpeg, Windows
with PowerShell. On a developer's Mac it films the tinyclient window alone with `screencapture`,
which needs Screen Recording for the terminal; the lab's Mac grants that to its helper only, so
macOS is not claimed (scenario.json).
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

OUT = Path(os.environ["E2E_OUT"]).resolve()
APP = Path(os.environ["E2E_APP_DIR"]).resolve()
T0 = float(os.environ.get("E2E_T0") or time.time())
WINDOWS = sys.platform == "win32"
MAC = sys.platform == "darwin"
NO_WINDOW = 0x08000000 if WINDOWS else 0

# The photo's box is 400 by 300 points, so each half is about 200 wide. A run this long (in
# screen pixels, at any scale) is no avatar or badge.
MIN_RUN = 100
MIN_ROWS = 60


def step(words, ok, shot=None):
    print(("[ok] " if ok else "[FAILED] ") + words, flush=True)
    line = {"t": round(time.time() - T0, 2), "step": words, "ok": bool(ok)}
    if shot:
        line["shot"] = shot
    with (OUT / "steps.jsonl").open("a", encoding="utf-8") as steps:
        steps.write(json.dumps(line) + "\n")
    if not ok:
        raise RuntimeError(words)


def until(look, seconds=25, every=0.25):
    end = time.monotonic() + seconds
    while True:
        found = look()
        if found or time.monotonic() >= end:
            return found
        time.sleep(every)


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


# --- screen pixels -----------------------------------------------------------------------------

GRAB_WINDOWS = r"""
Add-Type -Namespace Tiny -Name Dpi -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'
[Tiny.Dpi]::SetProcessDPIAware() | Out-Null
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
$bmp.Save($env:E2E_SHOT, [System.Drawing.Imaging.ImageFormat]::Bmp)
"""


# The largest ordinary on-screen window of a process, by its number. Needs no grant.
WINDOW_OF_MAC = """
ObjC.import('CoreGraphics');
function run(argv) {
  const pid = parseInt(argv[0], 10);
  const list = ObjC.castRefToObject($.CGWindowListCopyWindowInfo(1, 0));
  let best = '', area = 0;
  for (let i = 0; i < list.count; i++) {
    const w = list.objectAtIndex(i);
    if (w.objectForKey('kCGWindowOwnerPID').intValue !== pid || w.objectForKey('kCGWindowLayer').intValue !== 0) continue;
    const b = w.objectForKey('kCGWindowBounds');
    const a = b.objectForKey('Width').doubleValue * b.objectForKey('Height').doubleValue;
    if (a > area) { area = a; best = String(w.objectForKey('kCGWindowNumber').intValue); }
  }
  return best;
}
"""


def grab(name, pid):
    """The screen as an uncompressed BMP, which the standard library can read: on a Mac, the
    window of `pid` alone."""
    path = OUT / (name + ".bmp")
    path.unlink(missing_ok=True)
    if WINDOWS:
        subprocess.run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", GRAB_WINDOWS],
                       env=dict(os.environ, E2E_SHOT=str(path)), check=True, timeout=30, creationflags=NO_WINDOW,
                       capture_output=True)
    elif MAC:
        found = subprocess.run(["osascript", "-l", "JavaScript", "-e", WINDOW_OF_MAC, str(pid)],
                               capture_output=True, text=True, timeout=30)
        window = found.stdout.strip()
        if not window.isdigit():
            raise RuntimeError("the tinyclient window is not on screen")
        subprocess.run(["screencapture", "-x", "-o", "-l", window, "-t", "bmp", str(path)], check=True, timeout=30)
    else:
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "x11grab",
                        "-i", os.environ.get("DISPLAY", ":0"), "-frames:v", "1", str(path)], check=True, timeout=30)
    return path


class Frame:
    """An uncompressed 24 or 32 bit BMP: `rgb(x, y)` from the top left."""

    def __init__(self, data):
        if data[:2] != b"BM":
            raise RuntimeError("the screen grab is not a BMP")
        number = lambda at, size=4, signed=False: int.from_bytes(data[at:at + size], "little", signed=signed)
        start = number(10)
        self.width, height = number(18, signed=True), number(22, signed=True)
        bits, compression = number(28, 2), number(30)
        if bits not in (24, 32) or compression not in (0, 3):
            raise RuntimeError(f"a BMP of {bits} bits, compression {compression}, is not read here")
        masks = (0xFF0000, 0xFF00, 0xFF)
        if compression == 3:
            # After a 40-byte header, or inside a V4 or V5 header at the same place.
            masks = tuple(number(14 + 40 + 4 * i) for i in range(3))
        if any(mask not in (0xFF, 0xFF00, 0xFF0000, 0xFF000000) for mask in masks):
            raise RuntimeError(f"BMP channel masks {masks} are not whole bytes")
        self.channels = tuple((0xFF, 0xFF00, 0xFF0000, 0xFF000000).index(mask) for mask in masks)
        self.size = bits // 8
        self.stride = (self.width * bits + 31) // 32 * 4
        self.height = abs(height)
        self.top_down = height < 0
        self.data, self.start = data, start

    def row(self, y):
        at = y if self.top_down else self.height - 1 - y
        begin = self.start + at * self.stride
        return self.data[begin:begin + self.stride]

    def rgb(self, row, x):
        at = x * self.size
        r, g, b = self.channels
        return row[at + r], row[at + g], row[at + b]


def warm(r, g, b):
    return r >= 120 and r - b >= 80 and r - g >= 40 and g - b >= 10


def blue(r, g, b):
    return b >= 110 and b - r >= 70 and b - g >= 30


def dark(r, g, b):
    return max(r, g, b) < 90


def runs(frame, row, test, x0=0, x1=None, every=1):
    """Spans of `row` where `test` holds, as (start, end) with `end` exclusive, looking at every
    `every`th pixel."""
    x1 = frame.width if x1 is None else x1
    found, start = [], None
    for x in range(x0, x1, every):
        if test(*frame.rgb(row, x)):
            if start is None:
                start = x
        elif start is not None:
            found.append((start, x))
            start = None
    if start is not None:
        found.append((start, x1))
    return found


def halves(frame):
    """Where the photo's place shows a long orange run met by a long blue one, row after row:
    (left, top, right, bottom, middle), or None."""
    rows = []
    for y in range(0, frame.height, 2):
        row = frame.row(y)
        for start, end in runs(frame, row, warm, every=2):
            if end - start < MIN_RUN:
                continue
            reach = end + max(8, (end - start) // 6)
            after = [run for run in runs(frame, row, blue, end, min(frame.width, reach + 2 * (end - start)), every=2)
                     if run[0] <= reach and run[1] - run[0] >= MIN_RUN * 0.6]
            if after:
                rows.append((y, start, end, after[0][1]))
    # The longest stack of such rows lined up under each other is the photo.
    best, stack = [], []
    for y, left, middle, right in rows:
        if stack and (y - stack[-1][0] > 4 or abs(left - stack[-1][1]) > 6 or abs(right - stack[-1][3]) > 6):
            best, stack = max(best, stack, key=len), []
        stack.append((y, left, middle, right))
    best = max(best, stack, key=len)
    if not best or best[-1][0] - best[0][0] < MIN_ROWS:
        return None
    lefts, rights = sorted(r[1] for r in best), sorted(r[3] for r in best)
    middles = sorted(r[2] for r in best)
    return lefts[len(lefts) // 2], best[0][0], rights[len(rights) // 2], best[-1][0], middles[len(middles) // 2]


def stripes(frame, place):
    """Dark stripes across each half of `place`, on three rows: [(orange, blue), ...]."""
    left, top, right, bottom, middle = place
    counted = []
    for share in (0.3, 0.5, 0.7):
        row = frame.row(int(top + (bottom - top) * share))
        inset = max(4, (right - left) // 40)
        counted.append((len(runs(frame, row, dark, left + inset, middle - inset)),
                        len(runs(frame, row, dark, middle + inset, right - inset))))
    return counted


def edges(frame, place):
    """The outermost orange and blue on the middle row of `place`, a little beyond it."""
    left, top, right, bottom, _ = place
    row = frame.row((top + bottom) // 2)
    slack = max(12, (right - left) // 10)
    x0, x1 = max(0, left - slack), min(frame.width, right + slack)
    oranges, blues = runs(frame, row, warm, x0, x1), runs(frame, row, blue, x0, x1)
    if not oranges or not blues:
        return None
    return oranges[0][0], blues[-1][1]


# --- the run ------------------------------------------------------------------------------------

def run(data):
    folder = APP / "Contents" / "MacOS" if APP.suffix == ".app" else APP
    suffix = ".exe" if WINDOWS else ""
    chatd, tinyclient = (folder / (name + suffix) for name in ("chatd", "tinyclient"))
    env = {name: value for name, value in os.environ.items() if not name.startswith("TINYCLIENT_SESSION_")}
    env.update(TINYCLIENT_DIR=str(data), TINYCLIENT_TELEGRAM_FAKE="30", TINYCLIENT_TELEGRAM_T0=str(int(time.time())),
               TINYCLIENT_SESSION_TELEGRAM='{"user":7001,"name":"Mira Kessler","home":2,"dcs":[]}',
               TINYCLIENT_TELEGRAM_FILE_HOLD="1", TINYCLIENT_LOG_LEVEL="info")
    window = None
    with (OUT / "app.log").open("wb") as log:
        def spawn(*args):
            return subprocess.Popen([str(arg) for arg in args], cwd=data, env=env, stdout=log, stderr=log,
                                    creationflags=NO_WINDOW if args[0] == chatd else 0)

        def connected():
            result = subprocess.run([str(tinyclient), "--text"], cwd=data, env=env, capture_output=True, text=True,
                                    timeout=8, creationflags=NO_WINDOW)
            return "Telegram Connected" in result.stdout

        def steer(line):
            with (data / "telegram.ctl").open("a", encoding="utf-8") as control:
                control.write(line + "\n")

        def calls():
            path = data / "telegram.calls"
            return path.read_text(encoding="utf-8") if path.exists() else ""

        daemon = spawn(chatd)
        try:
            step("The invented Telegram account is connected", until(connected))
            steer("photo-preview")
            chat = json.dumps({"network": "telegram", "account": "7001", "id": "u7002"})
            window = spawn(tinyclient, "--open=" + chat)
            (OUT / "app.pid").write_text(str(window.pid), encoding="utf-8")
            step("Odalys's chat opens, and Telegram is asked for the photo's thumbnail and holds it",
                 until(lambda: "GetFile photo 6016 m" in calls(), 30))

            seen = {}

            def preview():
                shot = grab("01-preview", window.pid)
                frame = Frame(shot.read_bytes())
                place = halves(frame)
                if place and all(left == 0 and right == 0 for left, right in stripes(frame, place)):
                    seen.update(place=place, edges=edges(frame, place))
                    return shot
                return None

            # The wire holds the thumbnail until `file-go`, so what is on screen now came inside
            # the message.
            shot = until(preview, 20, 0.5)
            step("Before the thumbnail lands, the photo's place shows the preview from the message: "
                 "orange beside blue, without the thumbnail's stripes", bool(shot), "01-preview.bmp")

            steer("file-go")

            def sharp():
                shot = grab("02-sharp", window.pid)
                frame = Frame(shot.read_bytes())
                if all(left >= 8 and right >= 8 for left, right in stripes(frame, seen["place"])):
                    seen.update(sharp=edges(frame, seen["place"]))
                    return shot
                return None

            shot = until(sharp, 20, 0.5)
            step("When the thumbnail lands, its stripes replace the preview", bool(shot), "02-sharp.bmp")
            left, top, right, bottom, _ = seen["place"]
            slack = max(8, (right - left) // 25)
            before, after = seen.get("edges"), seen.get("sharp")
            same = bool(before and after and abs(before[0] - after[0]) <= slack and abs(before[1] - after[1]) <= slack)
            step(f"The thumbnail takes the preview's place at its size ({before} then {after}, "
                 f"box {right - left}x{bottom - top})", same)
            time.sleep(1.5)  # The last state stays on camera.
        finally:
            stop(window)
            stop(daemon)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    # A short path: the socket's name has to fit in a Unix socket address.
    with tempfile.TemporaryDirectory(prefix="tiny-tg-preview-", dir=None if WINDOWS else "/tmp") as directory:
        run(Path(directory))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr, flush=True)
        sys.exit(1)
