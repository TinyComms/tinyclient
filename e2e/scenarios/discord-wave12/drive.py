#!/usr/bin/env python3
"""Drive Discord account settings in the desktop app against invented fixture data."""
import csv
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager

import pyautogui

from fixture import TOKEN, discord_fixture

out = Path(os.environ['E2E_OUT'])
t0 = float(os.environ['E2E_T0'])
modifier = 'command' if sys.argv[1] == 'macos' else 'ctrl'
scale = (1, 1)


def read_screen(name):
    global scale
    path = out / (name + '.png')
    image = pyautogui.screenshot()
    image.save(path)
    width, height = pyautogui.size()
    scale = (width / image.width, height / image.height)
    ocr = subprocess.run(['tesseract', str(path), 'stdout', 'tsv'], check=True,
                         capture_output=True, text=True, encoding='utf-8')
    rows = csv.DictReader(io.StringIO(ocr.stdout), delimiter='\t')
    lines = {}
    for word in rows:
        if word['text'].strip():
            key = tuple(word[field] for field in ('block_num', 'par_num', 'line_num'))
            lines.setdefault(key, []).append(word)
    return path, list(lines.values())


def find(lines, words):
    wanted = words.lower().split()
    for line in lines:
        tokens = [word['text'].lower().strip('.,…:').removeprefix('https://') for word in line]
        for index in range(len(line) - len(wanted) + 1):
            last = index + len(wanted)
            if tokens[index:last] == wanted:
                start, end = line[index], line[last - 1]
                x = (int(start['left']) + int(end['left']) + int(end['width'])) // 2
                y = int(start['top']) + int(start['height']) // 2
                return round(x * scale[0]), round(y * scale[1])
    return None


def step(name, ok, path):
    with (out / 'steps.jsonl').open('a', encoding='utf-8') as steps:
        steps.write(json.dumps(dict(t=round(time.time() - t0, 2), step=name,
                                    ok=ok, shot=path.name)) + '\n')
    if not ok:
        raise RuntimeError(name)


def expect(name, present, absent=()):
    deadline = time.monotonic() + 15
    while True:
        path, lines = read_screen(name)
        ok = all(find(lines, text) for text in present) and not any(find(lines, text) for text in absent)
        if ok or time.monotonic() >= deadline:
            step(name, bool(ok), path)
            return path, lines
        time.sleep(0.4)


def press(name, words):
    _, lines = expect(name, [words])
    pyautogui.click(*find(lines, words))


def reveal(name, words, anchor, direction):
    _, lines = read_screen(name)
    point = find(lines, anchor)
    if point:
        pyautogui.moveTo(*point)
    deadline = time.monotonic() + 12
    while True:
        path, lines = read_screen(name)
        if all(find(lines, word) for word in words):
            step(name, True, path)
            return path, lines
        if time.monotonic() >= deadline:
            step(name, False, path)
        pyautogui.scroll(direction)
        time.sleep(0.25)


def drive():
    pyautogui.hotkey(modifier, ',')
    expect('01-discord-account-handoff', ['Accounts', 'Account on Discord', 'View devices'])
    press('02-open-devices', 'View devices')
    expect('03-current-and-other-devices', ['Wave Browser', 'This device', 'Wave Desktop'])
    press('04-request-device-sign-out', 'Sign out device')
    press('05-confirm-device-sign-out', 'Confirm sign-out')
    expect('06-other-device-signed-out', ['Wave Browser', 'This device'], ['Wave Desktop'])

    _, lines = reveal('07-find-authorized-apps', ['View apps'], 'Wave Browser', -1)
    pyautogui.click(*find(lines, 'View apps'))
    expect('08-authorized-app-is-listed', ['Wave Calendar'])
    press('09-request-revoke', 'Revoke access')
    press('10-confirm-revoke', 'Confirm revoke')
    expect('11-app-revoked', ['No authorized apps'], ['Wave Calendar'])

    _, lines = reveal('12-find-connections', ['View connections'], 'No authorized apps', -1)
    pyautogui.click(*find(lines, 'View connections'))
    expect('13-connection-is-listed', ['Wave Music'])
    press('14-request-disconnect', 'Disconnect')
    press('15-confirm-disconnect', 'Confirm disconnect')
    expect('16-connection-removed', ['No connections', 'Add a connection'], ['Wave Music'])

    path, lines = reveal('17-plan-and-handoff-rows', ['Family Centre', 'Nitro Basic', 'Shop', 'Quests'],
                         'No connections', 1)
    center = find(lines, 'Family Centre')
    open_buttons = [find([line], 'Open in Discord') for line in lines]
    beside = [point for point in open_buttons if point and point[0] > center[0] and abs(point[1] - center[1]) <= 20]
    step('18-family-centre-open-button', len(beside) == 1, path)
    pyautogui.click(*beside[0])
    pyautogui.hotkey(modifier, 'l')
    expect('19-family-centre-opens-in-discord', ['discord.com/feature/family-center'])


@contextmanager
def launch(binary, env, log_name, hidden=False):
    options = {}
    if hidden and sys.platform == 'win32':
        options['creationflags'] = subprocess.CREATE_NO_WINDOW
    with (out / log_name).open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT,
                                   **options)
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
    out.mkdir(parents=True, exist_ok=True)
    (out / 'steps.jsonl').write_text('', encoding='utf-8')
    app = Path(os.environ['E2E_APP_DIR'])
    if sys.argv[1] == 'macos':
        app = app / 'Contents' / 'MacOS'
    suffix = '.exe' if sys.argv[1] == 'windows' else ''
    scratch = '/tmp' if sys.argv[1] == 'macos' else None
    with discord_fixture() as port, tempfile.TemporaryDirectory(prefix='wave12-', dir=scratch) as data:
        env = dict(os.environ, TINYCLIENT_DIR=data,
                   TINYCLIENT_SESSION_DISCORD=json.dumps({'token': TOKEN}),
                   TINYCLIENT_DISCORD_API=f'http://127.0.0.1:{port}',
                   TINYCLIENT_DISCORD_GATEWAY=f'ws://127.0.0.1:{port}/?encoding=json&v=9',
                   TINYCLIENT_DISCORD_CDN=f'http://127.0.0.1:{port}/media')
        with launch(app / ('chatd' + suffix), env, 'chatd.log', hidden=True):
            with launch(app / ('tinyclient' + suffix), env, 'app.log') as process:
                (out / 'app.pid').write_text(str(process.pid), encoding='utf-8')
                expect('00-discord-account-connected', ['Wave Chat'])
                drive()


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(error, file=sys.stderr)
        sys.exit(1)
