#!/usr/bin/env python3
"""Drive the real desktop app against an invented Slack workspace: a huddle starts while the app
is signed in, its channel's row says who is in it, and the header offers to join it in Slack.

Env from the lab: E2E_OUT, E2E_T0, E2E_APP_DIR. The drivers use PyAutoGUI and Tesseract.
"""

from contextlib import contextmanager
import csv
import io
import json
import os
from pathlib import Path
import string
import subprocess
import sys
import tempfile
import time
import urllib.request

import pyautogui

from fixture import NAME, WORKSPACE, environment, huddle_started, huddle_updated, slack_fixture


OUT = Path(os.environ["E2E_OUT"])
T0 = float(os.environ["E2E_T0"])
PLATFORM = sys.argv[1]
# The channel's row, by the name it shows (without Slack's `#`).
ROW = NAME
# What the header offers while the huddle runs, then the number after it.
JOIN = "Join in Slack"
# What is cut from either end of a word the screen shows.
AROUND = string.punctuation + "·•–—"


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
        # Whatever OCR reads the dot of "Slack · 2" as, on its own or stuck to the 2, is not a word.
        text = word["text"].casefold().strip(AROUND)
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


def counted(lines, phrase, count):
    """Where a line says `phrase` and then `count`: the header's "Join in Slack · 2". What OCR
    makes of the dot between them is not relied on: the count follows at once, or a scrap of at
    most two characters stands between (never a word, so no other number is taken for it)."""
    wanted = phrase.casefold().split()
    for line in lines:
        words = [word[0] for word in line]
        for start in range(len(words) - len(wanted)):
            if words[start:start + len(wanted)] != wanted:
                continue
            after = words[start + len(wanted):]
            scrap = len(after[0]) <= 2 and not any(character.isdigit() for character in after[0])
            if after[0] == count or (scrap and len(after) > 1 and after[1] == count):
                yield (line[start][1], line[start][2])


def record(step, ok, shot):
    with (OUT / "steps.jsonl").open("a", encoding="utf-8") as steps:
        steps.write(json.dumps({"t": round(time.time() - T0, 2), "step": step, "ok": ok, "shot": shot}) + "\n")


def look_for(step, found, *phrases):
    """Screens the app until `found(lines)` holds, for 15 s. One step, one verdict."""
    shot = OUT / f"{step}.png"
    deadline = time.monotonic() + 15
    while True:
        lines = words_at_screen(shot)
        ok = found(lines)
        if ok or time.monotonic() >= deadline:
            break
        time.sleep(0.3)
    record(step, ok, shot.name)
    if not ok:
        raise RuntimeError(f"{step}: expected {', '.join(phrases)}")
    return lines


def shown(*phrases):
    """Every phrase is on screen, each on one line."""
    return lambda lines: all(next(matches(lines, phrase), None) for phrase in phrases)


def expect(step, *phrases):
    return look_for(step, shown(*phrases), *phrases)


def expect_count(step, phrase, count):
    return look_for(step, lambda lines: next(counted(lines, phrase, count), None) is not None, f"{phrase} then {count}")


def expect_gone(step, gone, *phrases):
    """The phrases are on screen and `gone` is nowhere: read again until both hold, so a pill
    that leaves a moment after the rest is not caught on its way out."""
    return look_for(step, lambda lines: shown(*phrases)(lines) and not next(matches(lines, gone), None),
                    *phrases, f"and no {gone}")


def tell(step, port, event):
    """The workspace says something on its socket, as Slack would."""
    shot = OUT / f"{step}.png"
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/socket-event", data=json.dumps(event).encode(),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as answer:
            ok = json.loads(answer.read()).get("ok") is True
    except (OSError, ValueError) as error:
        print(f"{step}: the workspace would not take the event: {error}", flush=True)
        ok = False
    words_at_screen(shot)
    record(step, ok, shot.name)
    if not ok:
        raise RuntimeError(f"{step}: the workspace would not take the event")


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


def drive(port):
    lines = expect("01-slack-workspace", WORKSPACE)
    if not next(matches(lines, ROW), None):
        pyautogui.click(*next(matches(lines, WORKSPACE)))
    lines = expect("02-the-channel-row-is-listed", ROW)

    # Slack's socket says a huddle started, with Ana and Ben in it.
    tell("03-the-workspace-starts-a-huddle", port, huddle_started())

    # The row says so before the conversation is opened. Its glyph is not something OCR can
    # read; hovering the row says who is in the huddle.
    row = next(matches(lines, ROW))
    pyautogui.moveTo(*row)
    expect("04-the-row-says-who-is-in-the-huddle", "in this huddle")

    # The header offers to join it in Slack and counts who is in it; the bar under the header
    # names the huddle and the two people.
    pyautogui.click(*row)
    expect("05-the-header-offers-joining-in-slack", JOIN)
    expect_count("06-the-header-counts-who-is-in-it", JOIN, "2")
    expect("07-the-huddle-names-who-is-in-it", "Huddle in progress", "Ana Ben")

    # One leaves: the count follows. Then it ends, and nothing offers joining it any more.
    tell("08-one-leaves", port, huddle_updated(["U0ANA"]))
    expect_count("09-the-header-counts-one", JOIN, "1")
    tell("10-the-huddle-ends", port, huddle_updated([], ended=True))
    expect_gone("11-nothing-offers-joining-it-any-more", JOIN, "Huddle ended")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "steps.jsonl").write_text("", encoding="utf-8")
    app = Path(os.environ["E2E_APP_DIR"]).resolve()
    if PLATFORM == "macos":
        app = app / "Contents" / "MacOS"
    suffix = ".exe" if PLATFORM == "windows" else ""
    scratch = "/tmp" if PLATFORM == "macos" else None
    with slack_fixture() as port, tempfile.TemporaryDirectory(prefix="huddle-live-", dir=scratch) as data:
        env = dict(os.environ, TINYCLIENT_DIR=data, **environment(port))
        with launch(app / ("chatd" + suffix), env, "chatd.log", hidden=True):
            with launch(app / ("tinyclient" + suffix), env, "app.log") as process:
                (OUT / "app.pid").write_text(str(process.pid), encoding="utf-8")
                drive(port)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, StopIteration, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
