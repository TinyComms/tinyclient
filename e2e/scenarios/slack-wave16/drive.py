#!/usr/bin/env python3
"""Drive the real desktop app against an invented Slack workspace."""

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

from fixture import WORKSPACE, environment, slack_fixture


OUT = Path(os.environ["E2E_OUT"])
T0 = float(os.environ["E2E_T0"])
PLATFORM = sys.argv[1]


def words_at_screen(shot):
    image = pyautogui.screenshot()
    image.save(shot)
    width, height = pyautogui.size()
    scale_x, scale_y = width / image.width, height / image.height
    response = subprocess.run(
        ["tesseract", str(shot), "stdout", "tsv"], capture_output=True, check=True, text=True, encoding="utf-8",
    )
    lines = {}
    for word in csv.DictReader(io.StringIO(response.stdout), delimiter="\t"):
        text = word["text"].casefold().strip(".,·•:;#?!")
        if text:
            line = tuple(word[key] for key in ("block_num", "par_num", "line_num"))
            x = int(word["left"]) + int(word["width"]) / 2
            y = int(word["top"]) + int(word["height"]) / 2
            lines.setdefault(line, []).append((text, round(x * scale_x), round(y * scale_y)))
    return list(lines.values())


def matches(lines, phrase):
    wanted = phrase.casefold().split()
    for line in lines:
        for start in range(len(line) - len(wanted) + 1):
            span = line[start:start + len(wanted)]
            if [word[0] for word in span] == wanted:
                yield ((span[0][1] + span[-1][1]) // 2, span[0][2])


def expect(step, *phrases):
    shot = OUT / f"{step}.png"
    deadline = time.monotonic() + 15
    while True:
        lines = words_at_screen(shot)
        if all(next(matches(lines, phrase), None) for phrase in phrases):
            ok = True
            break
        if time.monotonic() >= deadline:
            ok = False
            break
        time.sleep(0.3)
    with (OUT / "steps.jsonl").open("a", encoding="utf-8") as steps:
        steps.write(json.dumps({"t": round(time.time() - T0, 2), "step": step, "ok": ok, "shot": shot.name}) + "\n")
    if not ok:
        raise RuntimeError(f"{step}: expected {', '.join(phrases)}")
    return lines


@contextmanager
def launch(path, env, log, hidden=False):
    flags = subprocess.CREATE_NO_WINDOW if hidden and PLATFORM == "windows" else 0
    with (OUT / log).open("w", encoding="utf-8") as stream:
        process = subprocess.Popen([str(path)], env=env, stdout=stream, stderr=subprocess.STDOUT, creationflags=flags)
        try:
            yield process
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def drive():
    lines = expect("01-slack-workspace", WORKSPACE)
    if not next(matches(lines, "huddle-fixture"), None):
        pyautogui.click(*next(matches(lines, WORKSPACE)))
    lines = expect("02-huddle-channel", "huddle-fixture")
    pyautogui.click(*next(matches(lines, "huddle-fixture")))

    lines = expect(
        "03-huddle-summaries-and-notes",
        "Huddle in progress", "Join", "Huddle ended", "Started by Ana", "5 min", "2 participants",
        "3 replies", "Open thread", "Huddle notes", "Launch Recap", "launch Tuesday",
    )
    ended_y = next(matches(lines, "Huddle ended"))[1]
    thread = min((point for point in matches(lines, "Open thread") if point[1] > ended_y), key=lambda point: point[1])
    pyautogui.click(*thread)
    lines = expect("04-huddle-thread-opens-in-app", "Huddle thread", "Launch checklist ready", "Open in Slack")

    pyautogui.click(*next(matches(lines, "Open in Slack")))
    expect("05-confirm-the-notes-link", "Open this link")
    pyautogui.press("enter")
    expect("06-notes-canvas-opens-in-browser", "Huddle canvas fixture")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    app = Path(os.environ["E2E_APP_DIR"]).resolve()
    if PLATFORM == "macos":
        app = app / "Contents" / "MacOS"
    suffix = ".exe" if PLATFORM == "windows" else ""
    scratch = "/tmp" if PLATFORM == "macos" else None
    with slack_fixture() as port, tempfile.TemporaryDirectory(prefix="wave16-", dir=scratch) as data:
        env = dict(os.environ, TINYCLIENT_DIR=data, **environment(port))
        with launch(app / ("chatd" + suffix), env, "chatd.log", hidden=True):
            with launch(app / ("tinyclient" + suffix), env, "app.log") as process:
                (OUT / "app.pid").write_text(str(process.pid), encoding="utf-8")
                drive()


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, StopIteration, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
