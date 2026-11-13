#!/usr/bin/env python3
"""Drive the visible profile in the real app; the lab owns launch and recording."""
import csv
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import time

platform = sys.argv[1]
out = Path(os.environ['E2E_OUT'])
out.mkdir(parents=True, exist_ok=True)
t0 = float(os.environ['E2E_T0'])


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def capture(path):
    if platform == 'android':
        with path.open('wb') as image:
            run('adb', 'exec-out', 'screencap', '-p', stdout=image)
    elif platform == 'ios':
        run('xcrun', 'simctl', 'io', os.environ['E2E_UDID'], 'screenshot', str(path))
    else:
        import pyautogui
        pyautogui.screenshot().save(path)


def read_screen(name):
    path = out / (name + '.png')
    capture(path)
    result = run('tesseract', str(path), 'stdout', 'tsv', capture_output=True, text=True)
    rows = list(csv.DictReader(io.StringIO(result.stdout), delimiter='\t'))
    lines = {}
    for row in rows:
        if row['text'].strip():
            key = tuple(row[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
            lines.setdefault(key, []).append(row)
    return path, list(lines.values())


def find(lines, words):
    for line in lines:
        if words.lower() in ' '.join(word['text'] for word in line).lower():
            left = min(int(w['left']) for w in line)
            top = min(int(w['top']) for w in line)
            right = max(int(w['left']) + int(w['width']) for w in line)
            bottom = max(int(w['top']) + int(w['height']) for w in line)
            return ((left + right) // 2, (top + bottom) // 2)
    return None


def step(name, ok, path):
    with (out / 'steps.jsonl').open('a') as steps:
        steps.write(json.dumps(dict(t=round(time.time()-t0, 2), step=name,
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
        time.sleep(0.5)


def tap(point, path):
    x, y = point
    if platform == 'android':
        run('adb', 'shell', 'input', 'tap', str(x), str(y))
    elif platform == 'ios':
        from PIL import Image
        with Image.open(path) as image:
            width, height = image.size
        lab = Path(os.environ['E2E_LAB'])
        env = dict(os.environ, TEST_RUNNER_LAB_ACTIONS=f'tap {x/width} {y/height}; wait 1',
                   TEST_RUNNER_LAB_LOG=str(out / 'actions.log'), TEST_RUNNER_LAB_BUNDLE='dev.tinyclient.app')
        with (out / 'runner.log').open('a') as log:
            run('xcodebuild', 'test-without-building', '-project', str(lab / 'ios-ui/TinyLabUI.xcodeproj'),
                '-scheme', 'TinyLabUI', '-destination', 'platform=iOS Simulator,id=' + os.environ['E2E_UDID'],
                '-derivedDataPath', str(lab / 'ios-ui-dd'),
                '-only-testing:TinyLabUITests/LabActions/testActions', env=env, stdout=log, stderr=subprocess.STDOUT)
    else:
        import pyautogui
        pyautogui.click(x, y)


try:
    # Fail closed unless the fixture's invented stranger is the visible profile.
    path, lines = expect('01-stranger-profile', ['Casimir Thorne', 'Add friend'], ['Cancel request'])
    tap(find(lines, 'Add friend'), path)
    path, lines = expect('02-request-sent', ['Request sent', 'Cancel request'], ['Add friend'])
    tap(find(lines, 'Cancel request'), path)
    expect('03-request-cancelled', ['Add friend'], ['Cancel request', 'Request sent'])
except Exception as error:
    print(error, file=sys.stderr)
    sys.exit(1)
