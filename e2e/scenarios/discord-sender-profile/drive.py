#!/usr/bin/env python3
"""Click the visible message author and verify the same person's profile opens."""
import pathlib
import runpy
import sys

helpers = runpy.run_path(str(pathlib.Path(__file__).resolve().parent.parent /
                            'discord-profile-add-friend' / 'drive.py'))
expect, tap = (helpers[name] for name in ('expect', 'tap'))


def sender_position(lines):
    # The timestamp shares the sender's OCR line. Click only the name's bounds.
    wanted = ['Casimir', 'Thorne']
    for line in lines:
        for start in range(len(line) - len(wanted) + 1):
            words = line[start:start + len(wanted)]
            if [word['text'] for word in words] == wanted:
                left = min(int(word['left']) for word in words)
                top = min(int(word['top']) for word in words)
                right = max(int(word['left']) + int(word['width']) for word in words)
                bottom = max(int(word['top']) + int(word['height']) for word in words)
                return ((left + right) // 2, (top + bottom) // 2)
    raise RuntimeError('The fixture message sender is not visible')


try:
    path, lines = expect('01-message-author', ['Casimir Thorne'], ['View profile', 'Add friend'])
    tap(sender_position(lines), path)
    # Card on desktop, Profile page on a phone; both show the same handle and action.
    expect('02-sender-profile', ['@casimir', 'Add friend'])
except Exception as error:
    print(error, file=sys.stderr)
    sys.exit(1)
