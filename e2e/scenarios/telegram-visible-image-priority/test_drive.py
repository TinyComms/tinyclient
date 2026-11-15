#!/usr/bin/env python3
"""The driver's pixel check, on frames drawn here: `python3 test_drive.py`."""

import os
from pathlib import Path
import struct
import sys
import tempfile
import unittest

os.environ.setdefault("E2E_OUT", tempfile.gettempdir())
os.environ.setdefault("E2E_APP_DIR", tempfile.gettempdir())
sys.path.insert(0, str(Path(__file__).resolve().parent))
import drive  # noqa: E402

BACKGROUND = (30, 30, 34)
MAGENTA = (220, 40, 191)
GREEN = (41, 200, 90)


def bmp(width, height, paint, bits=24, top_down=False):
    """A BMP of `width` by `height` whose pixel at (x, y) is `paint(x, y)`."""
    size = bits // 8
    stride = (width * bits + 31) // 32 * 4
    rows = []
    for y in range(height):
        row = bytearray()
        for x in range(width):
            r, g, b = paint(x, y)
            row += bytes((b, g, r) + ((255,) if size == 4 else ()))
        row += b"\0" * (stride - len(row))
        rows.append(bytes(row))
    data = b"".join(rows if top_down else reversed(rows))
    header = struct.pack("<IiiHHIIiiII", 40, width, -height if top_down else height, 1, bits, 0, len(data), 2835, 2835, 0, 0)
    return b"BM" + struct.pack("<IHHI", 14 + 40 + len(data), 0, 0, 54) + header + data


def blend(a, b, share):
    return tuple(round(x + (y - x) * share) for x, y in zip(a, b))


def photo(left, top, width, height, blur=0):
    """The group's photo at this place: magenta, a green band across its middle third, with
    `blur` rows of mixed colour where they meet, as a scaled JPEG has."""
    band = (top + height // 3, top + 2 * height // 3)

    def paint(x, y):
        if not (left <= x < left + width and top <= y < top + height):
            return None
        for edge in band:
            if blur and abs(y - edge) < blur:
                inside = band[0] <= y < band[1]
                return blend(GREEN if inside else MAGENTA, BACKGROUND, 0.5)
        return GREEN if band[0] <= y < band[1] else MAGENTA

    return paint


def scene(*layers):
    def paint(x, y):
        for layer in layers:
            found = layer(x, y)
            if found is not None:
                return found
        return BACKGROUND
    return paint


def block(left, top, width, height, colour):
    return lambda x, y: colour if left <= x < left + width and top <= y < top + height else None


class GroupPhoto(unittest.TestCase):
    def found(self, paint, width=900, height=600, **kwargs):
        return drive.group_photo(drive.Frame(bmp(width, height, paint, **kwargs)))

    def test_the_photo_is_found_where_it_is_drawn(self):
        place = self.found(scene(photo(300, 120, 400, 300)))
        self.assertIsNotNone(place)
        left, top, right, bottom = place
        self.assertLessEqual(abs(left - 300), 4)
        self.assertLessEqual(abs(top - 120), 4)
        self.assertLessEqual(abs(right - 700), 4)
        self.assertLessEqual(abs(bottom - 419), 4)

    def test_a_smaller_photo_with_blurred_edges_and_a_top_down_32_bit_grab(self):
        self.assertIsNotNone(self.found(scene(photo(100, 50, 240, 180, blur=5)), bits=32, top_down=True))

    def test_what_is_not_the_photo_is_not_found(self):
        # The image icon's grey box, a magenta avatar, a green badge, and a magenta field
        # with no band: none of them is the photo.
        decoys = scene(block(300, 120, 400, 300, (70, 70, 76)), block(40, 40, 40, 40, MAGENTA),
                       block(120, 40, 60, 20, GREEN), block(100, 400, 300, 150, MAGENTA))
        self.assertIsNone(self.found(decoys))

    def test_only_a_band_beside_the_magenta_counts(self):
        # Green beside magenta, not across it.
        self.assertIsNone(self.found(scene(block(100, 100, 200, 200, MAGENTA), block(300, 100, 200, 200, GREEN))))


class ChannelThumb(unittest.TestCase):
    def test_a_piece_of_a_channel_thumbnail_and_nothing_else(self):
        self.assertTrue(drive.channel_thumb("GetFile photo 7100 m at 0"))
        self.assertTrue(drive.channel_thumb("GetFile photo 7129 m at 262144"))
        for call in ("GetFile photo 7111 y at 0", "GetFile photo 6020 m at 0", "GetFile avatar 7100", "GetHistory c9001 0 0"):
            self.assertFalse(drive.channel_thumb(call), call)


if __name__ == "__main__":
    unittest.main()
