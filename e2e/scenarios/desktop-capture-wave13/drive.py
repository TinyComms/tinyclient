#!/usr/bin/env python3
"""Exercises the shipped GUI and mediad in a disposable two-account call."""
import csv
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

TARGET = "Wave 13 red window"
RED = "#e02020"
VIOLET = "#a020e0"
GREEN = "#20e020"


def fixture(signal, geometry, title, color):
    import tkinter as tk

    window = tk.Tk()
    window.title(title)
    window.geometry(geometry)
    window.configure(bg=color)

    def apply():
        if signal.exists():
            verb, _, bounds = signal.read_text().strip().partition(" ")
            signal.unlink()
            if verb == "close":
                window.destroy()
                return
            if verb == "resize":
                window.geometry(bounds)
                window.configure(bg=VIOLET)
        window.after(60, apply)

    window.after(60, apply)
    window.mainloop()


def invoke(*args, **kwargs):
    return subprocess.run(args, check=True, timeout=30, **kwargs)


class Driver:
    def __init__(self):
        self.out = Path(os.environ["E2E_OUT"])
        self.start = float(os.environ["E2E_T0"])
        root = Path(os.environ["E2E_APP_DIR"])
        self.app = root / "Contents/MacOS" if root.suffix == ".app" else root
        config = Path(os.environ.get("E2E_DESKTOP_CAPTURE_FIXTURE",
                                     Path.home() / ".config/tiny-lab/desktop-capture-wave13.json"))
        self.config = json.loads(config.read_text())
        self.wayland = sys.platform.startswith("linux") and (
            os.environ.get("WAYLAND_DISPLAY") or os.environ.get("XDG_SESSION_TYPE") == "wayland"
        )
        self.children = []

    def spawn(self, *args, **kwargs):
        child = subprocess.Popen(args, **kwargs)
        self.children.append(child)
        return child

    def launch(self, role):
        data = self.out / role
        shutil.copytree(self.config[f"{role}_seed"], data)
        env = dict(os.environ, TINYCLIENT_DIR=str(data), TINYCLIENT_MEDIAD_AUDIO="tone:440")
        return self.spawn(str(self.app / "tinyclient"), env=env)

    def note(self, name, success, image=None):
        entry = {"t": round(time.time() - self.start, 2), "step": name, "ok": success}
        if image:
            entry["shot"] = image.name
        with (self.out / "steps.jsonl").open("a", encoding="utf-8") as steps:
            steps.write(json.dumps(entry) + "\n")
        if not success:
            raise AssertionError(name)

    def screenshot(self, name):
        path = self.out / f"{name}.png"
        if self.wayland:
            if path.exists():
                path.unlink()
            invoke("spectacle", "--background", "--nonotify", "--output", str(path))
        else:
            import pyautogui
            pyautogui.screenshot().save(path)
        return path

    def wait_for(self, name, predicate, deadline=18):
        end = time.monotonic() + deadline
        while True:
            path = self.screenshot(name)
            if result := predicate(path):
                self.note(name, True, path)
                return result
            if time.monotonic() >= end:
                self.note(name, False, path)
            time.sleep(0.35)

    @staticmethod
    def words(path, text):
        tsv = invoke("tesseract", str(path), "stdout", "tsv", capture_output=True, text=True).stdout
        lines = {}
        for row in csv.DictReader(io.StringIO(tsv), delimiter="\t"):
            if row["text"].strip():
                key = tuple(row[k] for k in ("block_num", "par_num", "line_num"))
                lines.setdefault(key, []).append(row)
        for line in lines.values():
            values = [row["text"] for row in line]
            for start in range(len(line)):
                for end in range(start + 1, len(line) + 1):
                    if " ".join(values[start:end]).casefold() != text.casefold():
                        continue
                    rows = line[start:end]
                    left = int(rows[0]["left"])
                    right = int(rows[-1]["left"]) + int(rows[-1]["width"])
                    top = max(int(row["top"]) for row in rows)
                    return ((left + right) // 2, top + int(rows[0]["height"]) // 2)
        return None

    def expect(self, text, name=None):
        return self.wait_for(name or text, lambda path: self.words(path, text))

    def hover(self, point):
        if self.wayland:
            x, y = map(str, point)
            invoke("ydotool", "mousemove", "--absolute", "-x", x, "-y", y)
        else:
            import pyautogui
            pyautogui.moveTo(*point)

    def click(self, point):
        self.hover(point)
        if self.wayland:
            invoke("ydotool", "click", "0xC0")
        else:
            import pyautogui
            pyautogui.click()

    def focus(self, process):
        if sys.platform == "darwin":
            invoke("osascript", "-e", f'tell application "System Events" to set frontmost of first process whose unix id is {process.pid} to true')
        else:
            found = invoke("xdotool", "search", "--onlyvisible", "--pid", str(process.pid),
                           capture_output=True, text=True).stdout.splitlines()
            invoke("xdotool", "windowactivate", "--sync", found[0])

    def join(self):
        self.click(self.expect(self.config["room"], "open the fixture voice room"))
        self.click(self.expect("Join channel", "join the fixture call"))

    @staticmethod
    def bounds(path, color):
        from PIL import Image

        image = Image.open(path).convert("RGB")
        target = tuple(bytes.fromhex(color[1:]))
        mask = Image.new("1", image.size)
        mask.putdata([max(abs(x - y) for x, y in zip(pixel, target)) < 35 for pixel in image.getdata()])
        return mask.getbbox()

    def inventory(self, sender):
        directory = self.out / "thumbnails"
        output = invoke(str(self.app / "mediad"), "--screens", str(directory),
                        env=dict(os.environ, TINYCLIENT_SHARE_PICKER_PID=str(sender.pid)), capture_output=True)
        sources = json.loads(output.stdout)
        monitors = [s for s in sources if s["id"].startswith("display:")]
        windows = [s for s in sources if s["id"].startswith("window:")]
        self.note("two different displays are listed", len({s["id"] for s in monitors}) >= 2)
        self.note("the covered fixture window has a thumbnail",
                  any(s["name"] == TARGET and self.bounds(s["thumbnail"], RED) for s in windows if "thumbnail" in s))
        second = next((s for s in monitors if not s.get("primary") and s.get("thumbnail")
                       and self.bounds(s["thumbnail"], GREEN)), None)
        self.note("the second display thumbnail contains its own fixture", second is not None)
        self.note("tinyclient windows are absent", not any(
            self.config["tiny_window_title"] == s["name"] for s in windows))
        return second["name"]

    def capture(self):
        red = self.out / "red.signal"
        self.spawn(sys.executable, __file__, "--fixture", str(red), self.config["window_geometry"], TARGET, RED)
        self.spawn(sys.executable, __file__, "--fixture", str(self.out / "green.signal"),
                   self.config["second_geometry"], "Wave 13 second display", GREEN)
        sender = self.launch("sender")
        (self.out / "app.pid").write_text(str(sender.pid))
        self.join()
        monitor = self.inventory(sender)

        # The lab fixture positions the call controls and the viewer to keep the red source
        # fully covered. Verify pixels in the receiver's video area, never anywhere on the desktop.
        self.click(self.config["share_button"])
        self.expect("Share your screen", "open the shared picker")
        self.click(self.expect(monitor, "choose the second display"))
        self.click(self.config["picker_share_button"])
        viewer = self.launch("viewer")
        self.join()
        self.click(self.expect("Watch stream", "watch the sender's chosen display"))
        self.wait_for("viewer sees the second display", lambda path: self.video_color(path, GREEN))

        self.focus(sender)
        self.hover(self.config["share_button"])
        self.expect("Stop sharing", "the sender is sharing the display")
        self.click(self.config["share_button"])
        self.click(self.config["share_button"])
        self.click(self.expect("Windows", "open the Windows tab"))
        self.click(self.expect(TARGET, "choose the covered application window"))
        self.click(self.config["picker_share_button"])
        self.focus(viewer)
        self.click(self.expect("Watch stream", "watch the sender's window"))
        self.wait_for("viewer sees the covered red window", lambda path: self.video_color(path, RED))

        red.write_text("resize " + self.config["resized_geometry"])
        self.wait_for("the resized window keeps producing frames", lambda path: self.video_color(path, VIOLET))
        red.write_text("close")
        self.focus(sender)
        self.hover(self.config["share_button"])
        self.expect("Share your screen", "closing the window ends the share")

    def video_color(self, path, color):
        from PIL import Image

        crop = self.out / "video.png"
        Image.open(path).crop(tuple(self.config["viewer_video_bounds"])).save(crop)
        bounds = self.bounds(crop, color)
        return bounds and (bounds[2] - bounds[0]) * (bounds[3] - bounds[1]) >= 2500

    def shortcuts(self):
        sender = self.launch("sender")
        (self.out / "app.pid").write_text(str(sender.pid))
        self.join()
        self.expect("Push to talk", "the fixture call uses push to talk")
        self.spawn(sys.executable, __file__, "--fixture", str(self.out / "focus.signal"),
                   self.config["focus_geometry"], "Other focused app", GREEN)
        self.click(self.expect("Other focused app", "focus an unrelated application"))

        # These are Linux input key codes for F8, F9, F10 and F11, bound in the fixture.
        for code, label in [(66, "Talking"), (67, "Priority speaking")]:
            invoke("ydotool", "key", f"{code}:1")
            try:
                self.expect(label, f"holding the shortcut shows {label} with another app focused")
            finally:
                invoke("ydotool", "key", f"{code}:0")
            self.wait_for(f"releasing clears {label}", lambda path: (
                self.words(path, "Push to talk") and not self.words(path, label)))

        invoke("ydotool", "key", "68:1", "68:0")
        self.expect("You're muted", "global mute changes the call")
        invoke("ydotool", "key", "68:1", "68:0")
        self.wait_for("global mute toggles back", lambda path: (
            self.words(path, "Push to talk") and not self.words(path, "You're muted")))
        invoke("ydotool", "key", "87:1", "87:0")
        self.expect("You're muted", "global deafen also mutes the call")
        self.click(self.config["sender_focus"])
        # The deafen control exposes the current state in its tooltip.
        self.hover(self.config["deafen_button"])
        self.expect("Undeafen", "the participant is deafened")

    def close(self):
        for child in reversed(self.children):
            if child.poll() is None:
                child.terminate()
        for child in self.children:
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--fixture":
        fixture(Path(sys.argv[2]), *sys.argv[3:])
    else:
        driver = Driver()
        try:
            driver.shortcuts() if driver.wayland else driver.capture()
        finally:
            driver.close()
