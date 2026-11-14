#!/usr/bin/env python3
"""Open the fixture DM and inspect its actual rendered acceptance message."""
import pathlib
import runpy
import sys

# Reuse the screen/OCR and native input adapters from the profile scenario.
helpers = runpy.run_path(str(pathlib.Path(__file__).resolve().parent.parent /
                            'discord-profile-add-friend' / 'drive.py'))
expect, find, tap = (helpers[name] for name in ('expect', 'find', 'tap'))
try:
    path, lines = expect('01-inbox', ['Casimir Thorne'])
    tap(find(lines, 'Casimir Thorne'), path)
    expect('02-friend-acceptance', ['Casimir Thorne', 'accepted your friend request'],
           ['A message tinyclient cannot show'])
except Exception as error:
    print(error, file=sys.stderr)
    sys.exit(1)
