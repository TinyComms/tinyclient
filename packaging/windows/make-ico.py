#!/usr/bin/env python3
"""Writes packaging/windows/tinyclient.ico, the installer's and shortcuts' icon, from the PNG
masters: icon-small.png (drawn from icon-small.svg) for 32 px and below, icon.png above. Each size
is stored as a PNG, which Windows reads at every size since Vista. Re-run it on a Mac when an
icon SVG changes and its PNG has been re-rendered:

    python3 packaging/windows/make-ico.py
"""
import os
import struct
import subprocess
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def render(size, out):
    master = "icon-small.png" if size <= 32 else "icon.png"
    subprocess.run(
        ["sips", "-z", str(size), str(size), os.path.join(ROOT, "packaging", master), "--out", out],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    with open(out, "rb") as f:
        return f.read()


def main():
    with tempfile.TemporaryDirectory() as tmp:
        images = [render(size, os.path.join(tmp, f"{size}.png")) for size in SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, data = b"", b""
    for size, png in zip(SIZES, images):
        # A width or height of 256 is written as 0.
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset + len(data))
        data += png
    out = os.path.join(ROOT, "packaging", "windows", "tinyclient.ico")
    with open(out, "wb") as f:
        f.write(header + entries + data)
    print(out)


if __name__ == "__main__":
    main()
