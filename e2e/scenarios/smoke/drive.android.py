"""Scenario `smoke`, Android driver. Runs on zeppelin and drives the emulator with adb only.

The app is the phone proof build (cargo feature `demo`): its made-up chatd answers from launch, so
the inbox shows invented people with nothing signed in and nothing on the network. This driver never
starts a server and never writes a session. See e2e/lab/platforms/android.py for how it is built.

Environment (set by the adapter): E2E_OUT, E2E_T0 (epoch seconds when the recorder started),
E2E_ADB, E2E_SERIAL, E2E_PACKAGE, E2E_SABOTAGE.

One line per step to $E2E_OUT/steps.jsonl: {"t": seconds since E2E_T0, "step": "...", "ok": bool}.
Exit 0 when every step passed, 1 at the first that did not.

E2E_SABOTAGE=1: the adapter builds the app without the demo feature, so there is no seed and the
inbox step has nothing to find. (The driver cannot take the seed out of an app that carries it, so
run by hand against a demo APK the sabotage switch changes nothing.)
"""
import json
import os
import subprocess
import sys
import time

OUT = os.environ["E2E_OUT"]
T0 = float(os.environ.get("E2E_T0") or time.time())
ADB = os.environ.get("E2E_ADB", "adb")
SERIAL = os.environ.get("E2E_SERIAL") or os.environ.get("ANDROID_SERIAL", "emulator-5554")
PKG = os.environ.get("E2E_PACKAGE", "dev.tinyclient.app")
FFMPEG = os.environ.get("E2E_FFMPEG_BIN", "ffmpeg")
STEPS = os.path.join(OUT, "steps.jsonl")
W, H = 24, 54  # the size a screenshot is shrunk to for comparing


def adb(*args, timeout=30, binary=False):
    done = subprocess.run([ADB, "-s", SERIAL, *args], capture_output=True, timeout=timeout)
    return done.stdout if binary else done.stdout.decode("utf-8", "replace").strip()


def sh(command, timeout=30):
    return adb("shell", command, timeout=timeout)


def step(words, ok, **extra):
    line = {"t": round(time.time() - T0, 2), "step": words, "ok": bool(ok)}
    line.update(extra)
    with open(STEPS, "a", encoding="utf-8") as f:
        f.write(json.dumps(line) + "\n")
    print(("ok   " if ok else "FAIL ") + words + ((" " + json.dumps(extra)) if extra else ""), flush=True)
    if not ok:
        wrap_up()
        sys.exit(1)


def wrap_up():
    try:
        sh("am force-stop " + PKG)
    except Exception:
        pass


def shot(name=None):
    """The screen as a tiny gray picture (W*H bytes). With a name, the full screenshot is kept too."""
    png = adb("exec-out", "screencap", "-p", binary=True, timeout=30)
    if name:
        with open(os.path.join(OUT, name), "wb") as f:
            f.write(png)
    done = subprocess.run([FFMPEG, "-hide_banner", "-loglevel", "error", "-i", "-", "-vf", "scale=%d:%d:flags=area" % (W, H),
                           "-pix_fmt", "gray", "-f", "rawvideo", "-"], input=png, capture_output=True, timeout=30)
    pixels = done.stdout
    return pixels if len(pixels) == W * H else b"\x00" * (W * H)


def spread(pixels):
    """How much the picture varies: standard deviation of the gray levels, and distinct levels."""
    mean = sum(pixels) / len(pixels)
    return (sum((p - mean) ** 2 for p in pixels) / len(pixels)) ** 0.5, len(set(p // 8 for p in pixels))


def moved(a, b):
    return sum(abs(x - y) for x, y in zip(a, b)) / len(a)


def moved_top(a, b, rows=14):
    """moved() over the top `rows` of the picture only: the Inbox title, its tabs and the search box."""
    return moved(a[:rows * W], b[:rows * W])


def settle(limit=20.0, quiet=1.5):
    """Wait until two looks, `quiet` seconds apart, are the same. Returns the last picture."""
    end = time.time() + limit
    last = shot()
    while time.time() < end:
        time.sleep(quiet)
        now = shot()
        if moved(last, now) < 0.6:
            return now
        last = now
    return last


def wait_loaded(limit=45.0):
    """Wait for a screen that has real content (not placeholders) and has stopped moving."""
    end = time.time() + limit
    now = settle(limit=min(limit, 15.0))
    while time.time() < end:
        contrast, levels = spread(now)
        if contrast > 14 and levels >= 14:
            return now, True
        time.sleep(1.0)
        now = shot()
    return now, False


def foreground():
    return sh("dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'", timeout=30)


def crashed(pid=""):
    """Crash-buffer lines that are this app's: it is named, or the line is about its process id. A fresh
    emulator's own Bluetooth stack aborts in a loop; that is not the app."""
    log = adb("logcat", "-d", "-b", "crash", timeout=30)
    mine = [PKG] + (["pid %s (" % pid] if pid else [])
    return [line[:160] for line in log.splitlines() if any(m in line for m in mine)][:3]


def swipe(y_from, y_to, ms=450):
    # (a fling follows the drag; the next one starts from the new position)
    adb("shell", "input", "swipe", "540", str(y_from), "540", str(y_to), str(ms))
    time.sleep(0.1)


def main():
    os.makedirs(OUT, exist_ok=True)
    print("driver started %.2fs after the recorder" % (time.time() - T0), flush=True)

    # 1. Nothing real in the app: no session file, so any data on screen is the demo's.
    files = sh("run-as %s ls files" % PKG)
    step("the app holds no account, so any people on screen are invented", "sessions.json" not in files, files=files.split()[:6])

    # 2. Launch the way a launcher icon does.
    sh("am start -W -n %s/.TinyActivity" % PKG, timeout=60)
    pid = ""
    for _ in range(30):
        pid = sh("pidof " + PKG)
        if pid and PKG in foreground():
            break
        time.sleep(1)
    step("the app launched and has the screen", bool(pid) and PKG in foreground(), pid=pid)

    # 3. The inbox. It draws grey placeholder rows first and the demo's people a few seconds later.
    shown, loaded = wait_loaded()
    spread_now, levels = spread(shown)
    inbox = shot("01-inbox.png")
    photos = sh("run-as %s find . -path '*demo-photos*' -type f" % PKG).split()
    ok = loaded and len(photos) >= 5
    step("the inbox lists the demo conversations", ok, demo_photos=len(photos), contrast=round(spread_now, 1), tones=levels)

    # Scroll down through more of them and back: the list is longer than the screen. Slow drags,
    # as a person scrolls, so the picture keeps moving; each `input` call costs about a second on
    # the emulator, so many quick swipes would be still more than they are moving.
    swipe(1900, 500, 3500)
    lower = shot()
    swipe(1900, 500, 3500)
    swipe(500, 1900, 3500)
    swipe(500, 1900, 3500)
    swipe(500, 1900, 1500)
    top = settle(limit=8.0)
    step("the inbox scrolls through more demo conversations and back", moved(inbox, lower) > 6.0 and moved_top(inbox, top) < 6.0,
         scrolled=round(moved(inbox, lower), 1), header_back=round(moved_top(inbox, top), 1))

    # 4. Open a conversation: the top row (whichever is there; the list is back at the top). The composer takes focus and raises the keyboard; Back
    #    puts that away and leaves the conversation, with its messages, on screen.
    adb("shell", "input", "tap", "600", "520")
    time.sleep(2.0)
    if "mInputShown=true" in sh("dumpsys input_method | grep mInputShown"):
        adb("shell", "input", "keyevent", "KEYCODE_BACK")
        time.sleep(1.0)
    opened = settle(limit=20.0)
    shot("02-conversation.png")
    changed = moved(inbox, opened)
    contrast, tones = spread(opened)
    step("a conversation opens", changed > 8.0 and tones >= 6, changed=round(changed, 1), tones=tones)

    # 5. Back to the inbox, then close.
    adb("shell", "input", "keyevent", "KEYCODE_BACK")
    time.sleep(2.0)
    back = settle()
    shot("03-back.png")
    # The window insets shift the list a few pixels after the keyboard has been up, so "the inbox"
    # means: its header (title, tabs, search) matches the inbox's and not the conversation's.
    near, far = moved_top(inbox, back), moved_top(opened, back)
    step("back returns to the inbox", near < 6.0 and far > 6.0, header_vs_inbox=round(near, 1), header_vs_conversation=round(far, 1))
    adb("shell", "input", "keyevent", "KEYCODE_BACK")
    time.sleep(1.5)
    sh("am force-stop " + PKG)
    time.sleep(1.0)
    gone = not sh("pidof " + PKG)
    bad = crashed(pid)
    step("the app closes cleanly", gone and not bad, crashes=bad)


if __name__ == "__main__":
    main()
