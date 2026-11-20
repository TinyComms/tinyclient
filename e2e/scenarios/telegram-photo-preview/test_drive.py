"""The driver's reading of screen pixels, on screens drawn here. Standard library only.

    python -m unittest discover -s e2e/scenarios/telegram-photo-preview -p "test_*.py"
"""

import os
from pathlib import Path
import sys
import tempfile
import unittest

os.environ.setdefault("E2E_OUT", tempfile.gettempdir())
os.environ.setdefault("E2E_APP_DIR", tempfile.gettempdir())
sys.path.insert(0, str(Path(__file__).resolve().parent))
import drive  # noqa: E402

GREY, ORANGE, BLUE, STRIPE = (236, 236, 238), (232, 119, 46), (31, 95, 191), (24, 24, 24)
SOFT_ORANGE, SOFT_BLUE = (180, 94, 37), (28, 81, 149)


def screen(width=1280, height=800):
    return [[GREY] * width for _ in range(height)]


def box(pixels, left, top, width, height, color):
    for y in range(top, top + height):
        pixels[y][left:left + width] = [color] * width


def photo(pixels, left, top, sharp, scale=1):
    width, height = 400 * scale, 300 * scale
    half = width // 2
    for y in range(top, top + height):
        row = pixels[y]
        for x in range(width):
            if sharp and (x // scale) % 10 < 2:
                row[left + x] = STRIPE
            elif sharp:
                row[left + x] = ORANGE if x < half else BLUE
            else:
                # The stretched preview: soft, with a blurred seam between the halves.
                at = (x - half + 12 * scale) / (24 * scale)
                mix = min(1.0, max(0.0, at))
                row[left + x] = tuple(round(a * (1 - mix) + b * mix) for a, b in zip(SOFT_ORANGE, SOFT_BLUE))


def bmp(pixels, bits=24, top_down=False):
    height, width = len(pixels), len(pixels[0])
    stride = (width * bits + 31) // 32 * 4
    rows = []
    for y in (range(height) if top_down else range(height - 1, -1, -1)):
        line = bytearray()
        for r, g, b in pixels[y]:
            line += bytes((b, g, r)) if bits == 24 else bytes((b, g, r, 255))
        rows.append(bytes(line) + bytes(stride - len(line)))
    body = b"".join(rows)
    masks = b"" if bits == 24 else (0xFF0000).to_bytes(4, "little") + (0xFF00).to_bytes(4, "little") + (0xFF).to_bytes(4, "little")
    start = 14 + 40 + len(masks)
    header = (b"BM" + (start + len(body)).to_bytes(4, "little") + bytes(4) + start.to_bytes(4, "little")
              + (40).to_bytes(4, "little") + width.to_bytes(4, "little")
              + (-height if top_down else height).to_bytes(4, "little", signed=True)
              + (1).to_bytes(2, "little") + bits.to_bytes(2, "little") + (0 if bits == 24 else 3).to_bytes(4, "little")
              + len(body).to_bytes(4, "little") + bytes(16))
    return header + masks + body


class ScreenTests(unittest.TestCase):
    def test_a_placeholder_and_an_orange_avatar_are_not_the_preview(self):
        pixels = screen()
        box(pixels, 300, 100, 40, 40, ORANGE)
        box(pixels, 340, 100, 40, 40, BLUE)
        box(pixels, 500, 300, 400, 300, (220, 220, 225))
        self.assertIsNone(drive.halves(drive.Frame(bmp(pixels))))

    def test_the_preview_is_found_soft_and_the_thumbnail_striped_in_the_same_place(self):
        pixels = screen()
        photo(pixels, 500, 300, sharp=False)
        soft = drive.Frame(bmp(pixels))
        place = drive.halves(soft)
        self.assertIsNotNone(place)
        left, top, right, bottom, middle = place
        self.assertLessEqual(abs(left - 500), 2)
        self.assertLessEqual(abs(right - 900), 2)
        self.assertGreaterEqual(bottom - top, 290)
        self.assertLessEqual(abs(middle - 700), 16)
        self.assertEqual(drive.stripes(soft, place), [(0, 0)] * 3)

        photo(pixels, 500, 300, sharp=True)
        sharp = drive.Frame(bmp(pixels))
        self.assertTrue(all(left >= 8 and right >= 8 for left, right in drive.stripes(sharp, place)))
        before, after = drive.edges(soft, place), drive.edges(sharp, place)
        self.assertLessEqual(abs(before[0] - after[0]), 4)
        self.assertLessEqual(abs(before[1] - after[1]), 4)

    def test_a_retina_screen_in_a_32_bit_top_down_bmp_reads_the_same(self):
        pixels = screen(1100, 800)
        photo(pixels, 100, 80, sharp=False, scale=2)
        frame = drive.Frame(bmp(pixels, bits=32, top_down=True))
        place = drive.halves(frame)
        self.assertIsNotNone(place)
        self.assertLessEqual(abs(place[0] - 100), 2)
        self.assertLessEqual(abs(place[2] - 900), 2)


if __name__ == "__main__":
    unittest.main()
