#!/usr/bin/env python3
"""Watch the synthetic Discord stream in the desktop app and check the speaker output."""

import array
from contextlib import contextmanager
import csv
import io
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from threading import Thread
import time

import pyautogui

from media_fixture import MediaFixture, TOKEN

OUT = Path(os.environ['E2E_OUT'])
T0 = float(os.environ['E2E_T0'])


def read_screen(name):
    path = OUT / (name + '.png')
    image = pyautogui.screenshot()
    image.save(path)
    width, height = pyautogui.size()
    tsv = subprocess.run(['tesseract', str(path), 'stdout', 'tsv'], check=True,
                         capture_output=True, text=True, encoding='utf-8').stdout
    lines = {}
    for word in csv.DictReader(io.StringIO(tsv), delimiter='\t'):
        text = word['text'].strip(' .,·…:').lower()
        if not text:
            continue
        line = tuple(word[key] for key in ('block_num', 'par_num', 'line_num'))
        x = int(word['left']) + int(word['width']) / 2
        y = int(word['top']) + int(word['height']) / 2
        lines.setdefault(line, []).append((text, x * width / image.width, y * height / image.height))
    return path, lines.values()


def find(lines, words):
    wanted = words.lower().split()
    for line in lines:
        for index in range(len(line) - len(wanted) + 1):
            run = line[index:index + len(wanted)]
            if [text for text, _, _ in run] == wanted:
                return round(sum(item[1] for item in run) / len(run)), round(run[0][2])
    return None


def step(name, ok, path):
    with (OUT / 'steps.jsonl').open('a', encoding='utf-8') as steps:
        steps.write(json.dumps(dict(t=round(time.time() - T0, 2), step=name,
                                    ok=ok, shot=path.name)) + '\n')
    if not ok:
        raise RuntimeError(name)


def expect(name, words):
    deadline = time.monotonic() + 15
    while True:
        path, lines = read_screen(name)
        ok = all(find(lines, word) for word in words)
        if ok or time.monotonic() >= deadline:
            step(name, ok, path)
            return lines
        time.sleep(0.4)


def press(name, words):
    pyautogui.click(*find(expect(name, [words]), words))


@contextmanager
def launch(binary, env, log_name, hidden=False):
    options = {'creationflags': subprocess.CREATE_NO_WINDOW} if hidden and sys.platform == 'win32' else {}
    with (OUT / log_name).open('w', encoding='utf-8') as log:
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


@contextmanager
def discord_media():
    with MediaFixture() as fixture:
        thread = Thread(target=fixture.serve_forever, daemon=True)
        thread.start()
        try:
            yield fixture.server_port
        finally:
            fixture.shutdown()
            thread.join()


def tone_level(data):
    samples = array.array('h')
    samples.frombytes(data[:len(data) - len(data) % 4])
    if sys.byteorder != 'little':
        samples.byteswap()
    mono = samples[::2]
    if not mono:
        return 0.
    coefficient = 2 * math.cos(2 * math.pi * 440 / 48000)
    first = second = 0.
    for value in mono:
        first, second = value + coefficient * first - second, first
    return math.sqrt(max(0., first * first + second * second - coefficient * first * second)) / len(mono)


def check_audio(name, sink, audible):
    start = sink.stat().st_size if sink.exists() else 0
    # Allow the gain ramp and one buffer to drain. Collect one second at the new setting.
    deadline = time.monotonic() + 6
    at = start + 48000  # 250 ms at 48 kHz, stereo signed 16-bit PCM.
    until = at + 192000
    while time.monotonic() < deadline:
        if sink.exists() and sink.stat().st_size >= until:
            break
        time.sleep(0.1)
    data = sink.read_bytes()[at:until] if sink.exists() else b''
    level = tone_level(data)
    print(f'{name}: 440 Hz amplitude {level:.1f}', flush=True)
    path, _ = read_screen(name)
    step(name, len(data) == until - at and (level > 500. if audible else level < 60.), path)


def open_call():
    name = '02-join-direct-call'
    deadline = time.monotonic() + 15
    while True:
        path, lines = read_screen(name)
        point = find(lines, 'Join call') or find(lines, 'Call')
        if point or time.monotonic() >= deadline:
            step(name, bool(point), path)
            pyautogui.click(*point)
            return
        time.sleep(0.4)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / 'steps.jsonl').write_text('', encoding='utf-8')
    sink = OUT / 'played.pcm'
    sink.unlink(missing_ok=True)
    platform = sys.argv[1]
    app = Path(os.environ['E2E_APP_DIR'])
    if platform == 'macos':
        app = app / 'Contents' / 'MacOS'
    suffix = '.exe' if platform == 'windows' else ''
    scratch = '/tmp' if platform == 'macos' else None
    with discord_media() as port, tempfile.TemporaryDirectory(prefix='wave15-', dir=scratch) as data:
        origin = f'127.0.0.1:{port}'
        env = dict(os.environ, TINYCLIENT_DIR=data,
                   TINYCLIENT_SESSION_DISCORD=json.dumps({'token': TOKEN}),
                   TINYCLIENT_DISCORD_API=f'http://{origin}',
                   TINYCLIENT_DISCORD_GATEWAY=f'ws://{origin}/?encoding=json&v=9',
                   TINYCLIENT_DISCORD_CDN=f'http://{origin}/media',
                   TINYCLIENT_VOICE_PLAIN='1', TINYCLIENT_MEDIAD_AUDIO='null',
                   TINYCLIENT_MEDIAD_SINK=str(sink.resolve()))
        with launch(app / ('chatd' + suffix), env, 'chatd.log', hidden=True):
            with launch(app / ('tinyclient' + suffix), env, 'app.log') as process:
                (OUT / 'app.pid').write_text(str(process.pid), encoding='utf-8')
                press('01-open-discord-direct', 'Wave Chat')
                open_call()
                press('03-watch-the-discord-stream', 'Watch')
                expect('04-stream-has-separate-volume', ['Stream volume', 'Mute stream'])
                check_audio('05-the-stream-is-audible', sink, True)
                press('06-mute-stream', 'Mute stream')
                expect('07-stream-mute-is-on', ['Unmute stream'])
                check_audio('08-the-stream-is-silent', sink, False)
                press('09-unmute-stream', 'Unmute stream')
                expect('10-stream-mute-is-off', ['Mute stream'])
                check_audio('11-the-stream-is-audible-again', sink, True)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(error, file=sys.stderr)
        sys.exit(1)
