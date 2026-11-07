"""Windows pixel-oracle integration; no app/IPC substitutes for image text."""

import base64
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import native_windows


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


if __name__ == "__main__":
    unittest.main()
