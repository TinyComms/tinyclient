#!/usr/bin/env python3
"""Drive the installed UI with desktop input and check what is visible on screen."""

from contextlib import contextmanager
import csv
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import pyautogui

from fixture import teams_fixture

OUT = Path(os.environ["E2E_OUT"])
T0 = float(os.environ["E2E_T0"])
PLATFORM = sys.argv[1]


def screen(name):
    path = OUT / f"{name}.png"
    picture = pyautogui.screenshot()
    picture.save(path)
    width, height = pyautogui.size()
    scale = width / picture.width, height / picture.height
    result = subprocess.run(
        ["tesseract", str(path), "stdout", "tsv"], check=True, capture_output=True, encoding="utf-8",
    )
    lines = {}
    for word in csv.DictReader(io.StringIO(result.stdout), delimiter="\t"):
        if word["text"].strip():
            line = tuple(word[field] for field in ("block_num", "par_num", "line_num"))
            lines.setdefault(line, []).append(word)
    return path, list(lines.values()), scale


def position(lines, scale, phrase):
    wanted = phrase.casefold().split()
    for line in lines:
        words = [word["text"].casefold().strip(".,…:") for word in line]
        for start in range(len(words) - len(wanted) + 1):
            end = start + len(wanted)
            if words[start:end] != wanted:
                continue
            first, last = line[start], line[end - 1]
            center_x = (int(first["left"]) + int(last["left"]) + int(last["width"])) / 2
            center_y = int(first["top"]) + int(first["height"]) / 2
            return round(center_x * scale[0]), round(center_y * scale[1])
    return None


def expect(name, *phrases):
    deadline = time.monotonic() + 24
    while True:
        shot, lines, scale = screen(name)
        points = [position(lines, scale, phrase) for phrase in phrases]
        ok = all(points)
        if ok or time.monotonic() >= deadline:
            with (OUT / "steps.jsonl").open("a", encoding="utf-8") as steps:
                steps.write(json.dumps({"t": round(time.time() - T0, 2), "step": name, "ok": ok, "shot": shot.name}) + "\n")
            if not ok:
                raise RuntimeError(f"{name}: missing {[word for word, point in zip(phrases, points) if not point]}")
            return points
        time.sleep(0.3)


def press(name, words):
    point, = expect(name, words)
    pyautogui.click(*point)


def drive():
    expect("00-account-connected", "Wave Review")
    pyautogui.hotkey("command" if PLATFORM == "macos" else "ctrl", "3")
    press("01-open-Teams-chat", "Wave Review")
    expect("02-recording-card", "Launch recording", "12:34", "Open recording")
    press("03-read-transcript", "Read transcript")
    expect("04-speaker-and-transcript", "Ana Ruiz Ready for launch")
    pyautogui.press("escape")
    press("05-open-the-Calls-view", "Calls")
    expect("06-missed-call-and-Exchange-voicemail", "Missed", "Voicemail", "Please call at noon")
    press("07-return-to-chat", "Wave Review")
    press("08-open-meeting-options", "Meet")
    press("09-create-instant-meeting", "Meet now")
    expect("10-instant-meeting-link", "Meeting created", "Copy link", "Open in Teams")
    press("11-close-meeting-result", "Done")
    expect("12-instant-meeting-post", "Join meeting")
    press("13-open-meeting-options", "Meet")
    press("14-open-schedule-form", "Schedule")
    expect("15-calendar-form", "Start", "End", "Attendee emails")
    press("16-confirm-schedule", "Confirm and schedule")
    expect("17-scheduled-meeting-link", "Meeting created", "Copy link")
    press("18-close-scheduled-result", "Done")
    expect("19-scheduled-meeting-post", "Scheduled meeting", "Join meeting")


@contextmanager
def launch(binary, env, log_name, hidden=False):
    flags = subprocess.CREATE_NO_WINDOW if PLATFORM == "windows" and hidden else 0
    with (OUT / log_name).open("w", encoding="utf-8") as log:
        process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT, creationflags=flags)
        try:
            yield process
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    app = Path(os.environ["E2E_APP_DIR"])
    if PLATFORM == "macos" and app.suffix == ".app":
        app = app / "Contents" / "MacOS"
    suffix = ".exe" if PLATFORM == "windows" else ""
    scratch = "/tmp" if PLATFORM == "macos" else None
    with teams_fixture() as fixture, tempfile.TemporaryDirectory(prefix="wave15-", dir=scratch) as data:
        env = {key: value for key, value in os.environ.items() if not key.startswith("TINYCLIENT_SESSION_")}
        env.update(TINYCLIENT_DIR=data, **fixture)
        with launch(app / ("chatd" + suffix), env, "chatd.log", hidden=True):
            with launch(app / ("tinyclient" + suffix), env, "app.log") as process:
                (OUT / "app.pid").write_text(str(process.pid), encoding="utf-8")
                drive()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(error, file=sys.stderr)
        sys.exit(1)
