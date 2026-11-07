"""Windows pixel-oracle integration; no app/IPC substitutes for image text."""

import base64
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import native_windows


class SidebarLocatorTests(unittest.TestCase):
    def test_only_unique_visible_sidebar_name_can_be_clicked(self):
        words = [
            {"text": "Bea", "x": 50, "y": 100, "width": 30, "height": 20},
            {"text": "Bea", "x": 400, "y": 100, "width": 30, "height": 20},
        ]
        self.assertEqual(native_windows.sidebar_word_center(words, 1024, "Bea"), (65, 110))
        for ambiguous in ([], words[:1] * 2):
            with self.subTest(words=ambiguous), self.assertRaises(native_windows.NativeError):
                native_windows.sidebar_word_center(ambiguous, 1024, "Bea")


@unittest.skipUnless(sys.platform == "win32", "requires Windows OCR")
class PixelTests(unittest.TestCase):
    def test_reads_actual_png_and_rejects_an_absent_marker_without_tesseract(self):
        with tempfile.TemporaryDirectory() as scratch:
            image = Path(scratch) / "pixels.png"
            path = str(image).replace("'", "''")
            script = f"""
Add-Type -AssemblyName System.Drawing
$bmp=New-Object Drawing.Bitmap 1100,200
$g=[Drawing.Graphics]::FromImage($bmp)
$g.Clear([Drawing.Color]::White)
$font=New-Object Drawing.Font 'Segoe UI',26
$g.DrawString('opened-history-cache-commit-fern-clover',$font,[Drawing.Brushes]::Black,20,60)
$g.DrawString('Bea',$font,[Drawing.Brushes]::Black,20,120)
$g.DrawString('Bea',$font,[Drawing.Brushes]::Black,500,120)
$bmp.Save('{path}',[Drawing.Imaging.ImageFormat]::Png)
$font.Dispose(); $g.Dispose(); $bmp.Dispose()
"""
            encoded = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
            subprocess.run(["powershell.exe", "-NoProfile", "-EncodedCommand", encoded],
                           check=True, capture_output=True, timeout=15)
            self.assertIs(native_windows.png_mentions_marker(str(image),
                          "opened-history-cache-commit-fern-clover"), True)
            self.assertIs(native_windows.png_mentions_marker(str(image),
                          "opened-history-cache-commit-wrong-marker"), False)
            observed = native_windows.windows_ocr(str(image))
            x, y = native_windows.sidebar_word_center(observed["words"], 1100, "Bea")
            self.assertTrue(20 <= x <= 100 and 120 <= y <= 180)


if __name__ == "__main__":
    unittest.main()
