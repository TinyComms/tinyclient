#!/usr/bin/env python3
"""A Telegram chat opened after an image-heavy channel fetches its picture on screen first.

The fake Telegram wire writes 30 photo posts into the channel Field Notes and one photo into
the group Trail crew (`photo-pages 30`), as posts made while the app was closed: no thumbnail
is on disk. Telegram's files come one at a time, a quarter second apart, and each channel
thumbnail is two pieces, so the channel's page keeps the line busy for about fifteen seconds.
The group's thumbnail is a real picture, a field of magenta across a green band.

1. chatd signs in to the invented account, and the posts are written.
2. The real window opens Field Notes. Telegram is asked for its thumbnails, one after another.
3. Ctrl+K, "Trail crew", Enter: the group opens while most of the channel's thumbnails wait.
4. The wire's log (`telegram.calls`): the group's thumbnail was asked for before channel
   thumbnails that were already waiting when the group opened.
5. Screen pixels: the group's photo is drawn, magenta above and below a green band.

A build from before the change asks for the group's thumbnail only after every channel
thumbnail, so it fails at step 4. Its fake wire does not know `photo-pages` either, so the
base run stops at step 2.

Env from the lab: E2E_OUT, E2E_APP_DIR, E2E_T0. Linux grabs the X display with ffmpeg and types
through XTest; Windows grabs with PowerShell and types with SendKeys. On a developer's Mac it
films the tinyclient window alone with `screencapture` and types through System Events, which
need Screen Recording and Accessibility for the terminal; the lab's Mac grants those to its
helper only, so macOS is not claimed (scenario.json).
"""

import ctypes
import json
import os
from pathlib import Path
import shutil
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

POSTS = 30
CHANNEL = {"network": "telegram", "account": "7001", "id": "c9001"}
GROUP = "Trail crew"
GROUP_THUMB = "GetFile photo 6020 m"
# The photo's box is about 400 by 300 points, a third of it the green band. Runs this long
# (in screen pixels, at any scale) are no avatar, badge or icon.
MIN_RUN = 100
MIN_ROWS = 20


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


def channel_thumb(call):
    """A piece of one of the channel's photo thumbnails (`p7100m` up)."""
    return call.startswith("GetFile photo 71") and " m at " in call


# --- keys -------------------------------------------------------------------------------------

FRONT_WINDOWS = r"""
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class TinyFront {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public static uint Pid(IntPtr h) { uint p; GetWindowThreadProcessId(h, out p); return p; }
  public static IntPtr Find(uint p) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { if (Pid(h) == p && IsWindowVisible(h)) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
}
'@
Add-Type -AssemblyName System.Windows.Forms
$pid_ = [uint32]$env:E2E_PID
$window = [TinyFront]::Find($pid_)
if ($window -eq [IntPtr]::Zero) { throw "no window of $pid_" }
[TinyFront]::ShowWindow($window, 9) | Out-Null
if ([TinyFront]::GetForegroundWindow() -ne $window) {
  # A tap of Alt lets this process hand the foreground on, which Windows otherwise refuses.
  [TinyFront]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
  [TinyFront]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
  [TinyFront]::SetForegroundWindow($window) | Out-Null
}
Start-Sleep -Milliseconds 300
if ([TinyFront]::Pid([TinyFront]::GetForegroundWindow()) -ne $pid_) { throw "tinyclient is not in front; nothing typed" }
foreach ($keys in $env:E2E_KEYS -split "`n") {
  [System.Windows.Forms.SendKeys]::SendWait($keys)
  Start-Sleep -Milliseconds 600
}
"""


class X11:
    """Keys through XTest (libX11 and libXtst, which the desktop has), as a person's."""

    def __init__(self):
        self.x11 = ctypes.CDLL("libX11.so.6")
        self.xtst = ctypes.CDLL("libXtst.so.6")
        self.x11.XOpenDisplay.restype = ctypes.c_void_p
        self.x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
        self.x11.XStringToKeysym.restype = ctypes.c_ulong
        self.x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
        self.x11.XKeysymToKeycode.restype = ctypes.c_ubyte
        self.x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        self.x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self.xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.display = self.x11.XOpenDisplay(None)
        if not self.display:
            raise RuntimeError("cannot open the X display")

    def press(self, *names):
        codes = []
        for name in names:
            code = self.x11.XKeysymToKeycode(self.display, self.x11.XStringToKeysym(name.encode()))
            if not code:
                raise RuntimeError(f"no key called {name!r}")
            codes.append(code)
        for down in (1, 0):
            for code in codes if down else reversed(codes):
                self.xtst.XTestFakeKeyEvent(self.display, code, down, 0)
                self.x11.XSync(self.display, 0)
                time.sleep(0.03)

    def type(self, text):
        for ch in text:
            if ch == " ":
                self.press("space")
            elif ch.isupper():
                self.press("Shift_L", ch)
            else:
                self.press(ch)
            time.sleep(0.04)


def front_linux(pid):
    """The window of `pid` brought forward with wmctrl, when the host has it."""
    if not shutil.which("wmctrl"):
        return
    listed = subprocess.run(["wmctrl", "-lp"], capture_output=True, text=True, timeout=10).stdout
    for line in listed.splitlines():
        fields = line.split()
        # Owned by its pid, or titled for it, as the smoke driver finds it.
        if len(fields) > 2 and (fields[2] == str(pid) or "tinyclient" in line.lower()):
            subprocess.run(["wmctrl", "-ia", fields[0]], timeout=10)
            time.sleep(0.3)
            return


def jump(pid, name):
    """Ctrl+K (Cmd+K on a Mac), the name, Enter: the app's own quick switcher."""
    if WINDOWS:
        keys = "\n".join(["^k", "".join("{" + ch + "}" if ch in "+^%~(){}[]" else ch for ch in name), "{ENTER}"])
        subprocess.run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", FRONT_WINDOWS],
                       env=dict(os.environ, E2E_PID=str(pid), E2E_KEYS=keys), check=True, timeout=30,
                       creationflags=NO_WINDOW, capture_output=True)
    elif MAC:
        front = f'tell application "System Events" to set frontmost of (first process whose unix id is {pid}) to true'
        subprocess.run(["osascript", "-e", front], check=True, timeout=10)
        time.sleep(0.3)
        for line in ('keystroke "k" using command down', f'keystroke "{name}"', "key code 36"):
            subprocess.run(["osascript", "-e", f'tell application "System Events" to {line}'], check=True, timeout=10)
            time.sleep(0.6)
    else:
        front_linux(pid)
        keys = X11()
        keys.press("Control_L", "k")
        time.sleep(0.6)
        keys.type(name)
        time.sleep(0.6)
        keys.press("Return")


# --- screen pixels ----------------------------------------------------------------------------

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
    """An uncompressed 24 or 32 bit BMP: `rgb(row, x)` from the top left."""

    def __init__(self, data):
        if data[:2] != b"BM":
            raise RuntimeError("the screen grab is not a BMP")
        number = lambda at, size=4, signed=False: int.from_bytes(data[at:at + size], "little", signed=signed)
        self.start = number(10)
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
        self.data = data

    def row(self, y):
        at = y if self.top_down else self.height - 1 - y
        begin = self.start + at * self.stride
        return self.data[begin:begin + self.stride]

    def rgb(self, row, x):
        at = x * self.size
        r, g, b = self.channels
        return row[at + r], row[at + g], row[at + b]


def magenta(r, g, b):
    return r >= 160 and b >= 130 and g <= 110 and r - g >= 90 and b - g >= 70


def green(r, g, b):
    return g >= 140 and g - r >= 80 and g - b >= 50


def longest(frame, row, test):
    """The longest span of `row` where `test` holds, as (start, end), every other pixel."""
    best, start = (0, 0), None
    for x in range(0, frame.width, 2):
        if test(*frame.rgb(row, x)):
            start = x if start is None else start
        elif start is not None:
            best = max(best, (start, x), key=lambda span: span[1] - span[0])
            start = None
    if start is not None:
        best = max(best, (start, frame.width), key=lambda span: span[1] - span[0])
    return best


def group_photo(frame):
    """Where a magenta field with a green band across it stands, as (left, top, right, bottom),
    or None: magenta rows, then green rows, then magenta rows, over the same columns."""
    bands = []  # (colour, first y, last y, left, right)
    for y in range(0, frame.height, 2):
        row = frame.row(y)
        for colour, test in (("magenta", magenta), ("green", green)):
            left, right = longest(frame, row, test)
            if right - left < MIN_RUN:
                continue
            last = bands[-1] if bands else None
            if last and last[0] == colour and y - last[2] <= 4 and abs(left - last[3]) <= 8 and abs(right - last[4]) <= 8:
                bands[-1] = (colour, last[1], y, last[3], last[4])
            else:
                bands.append((colour, y, y, left, right))
    tall = [band for band in bands if (band[2] - band[1]) // 2 + 1 >= MIN_ROWS]
    for above, middle, below in zip(tall, tall[1:], tall[2:]):
        colours = (above[0], middle[0], below[0])
        lined_up = all(abs(band[3] - above[3]) <= 8 and abs(band[4] - above[4]) <= 8 for band in (middle, below))
        # A scaled JPEG blurs where the colours meet: a few rows there are neither.
        touching = middle[1] - above[2] <= 16 and below[1] - middle[2] <= 16
        if colours == ("magenta", "green", "magenta") and lined_up and touching:
            return above[3], above[1], above[4], below[2]
    return None


# --- the run ------------------------------------------------------------------------------------

def run(data):
    folder = APP / "Contents" / "MacOS" if APP.suffix == ".app" else APP
    suffix = ".exe" if WINDOWS else ""
    chatd, tinyclient = (folder / (name + suffix) for name in ("chatd", "tinyclient"))
    env = {name: value for name, value in os.environ.items() if not name.startswith("TINYCLIENT_SESSION_")}
    env.update(TINYCLIENT_DIR=str(data), TINYCLIENT_TELEGRAM_FAKE="30", TINYCLIENT_TELEGRAM_T0=str(int(time.time())),
               TINYCLIENT_SESSION_TELEGRAM='{"user":7001,"name":"Mira Kessler","home":2,"dcs":[]}',
               TINYCLIENT_LOG_LEVEL="info")
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
            return path.read_text(encoding="utf-8").splitlines() if path.exists() else []

        daemon = spawn(chatd)
        try:
            step("The invented Telegram account is connected", until(connected))
            steer(f"photo-pages {POSTS}")
            window = spawn(tinyclient, "--open=" + json.dumps(CHANNEL))
            (OUT / "app.pid").write_text(str(window.pid), encoding="utf-8")
            step(f"Field Notes opens, and Telegram is asked for its {POSTS} photos' thumbnails one at a time",
                 until(lambda: any(channel_thumb(call) for call in calls()), 30))
            time.sleep(1.5)  # The channel and its first thumbnails stay on camera.

            try:
                jump(window.pid, GROUP)
            except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                step(f"Ctrl+K, {GROUP}, Enter is typed into the window ({error})", False)
            opened = until(lambda: any(call.startswith("GetHistory g8001") for call in calls()), 20)
            asked = {call.split(" at ")[0] for call in calls() if channel_thumb(call)}
            step(f"{GROUP} opens through Ctrl+K while {POSTS - len(asked)} of the channel's thumbnails wait",
                 bool(opened) and len(asked) < POSTS)

            # Every thumbnail is asked for in the end; then the order they went in is whole.
            step(f"Telegram is asked for {GROUP}'s photo", bool(until(lambda: any(call.startswith(GROUP_THUMB) for call in calls()), 60)))
            until(lambda: sum(channel_thumb(call) for call in calls()) >= 2 * POSTS, 60)
            seen = calls()
            (OUT / "telegram.calls.txt").write_text("\n".join(seen) + "\n", encoding="utf-8")
            at = next(index for index, call in enumerate(seen) if call.startswith(GROUP_THUMB))
            after = {call.split(" at ")[0] for call in seen[at + 1:] if channel_thumb(call)}
            waiting = after - asked
            step(f"{GROUP}'s photo on screen was fetched ahead of {len(waiting)} channel thumbnails that were "
                 f"already waiting when it opened", len(waiting) > 0)

            def shown():
                shot = grab("01-group-photo", window.pid)
                return shot if group_photo(Frame(shot.read_bytes())) else None

            shot = until(shown, 20, 0.5)
            step(f"{GROUP}'s photo is drawn: magenta across a green band", bool(shot), "01-group-photo.bmp")
            time.sleep(1.5)  # The last state stays on camera.
        finally:
            stop(window)
            stop(daemon)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    # A short path: the socket's name has to fit in a Unix socket address.
    with tempfile.TemporaryDirectory(prefix="tiny-tg-order-", dir=None if WINDOWS else "/tmp") as directory:
        run(Path(directory))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.SubprocessError, StopIteration) as error:
        print(error, file=sys.stderr, flush=True)
        sys.exit(1)
