"""Recognize the four corners and number strip in the sender's full-screen card."""
import subprocess

COLORS = ("#dc2734", "#24be60", "#3265e4", "#f4cf31")
CORNERS = ((20, 20), (80, 20), (20, 80), (80, 80))
BIT_CENTERS = (35, 41, 47, 53, 59, 65)


def read_card(ffmpeg, source, rect, at=None):
    """Read from a PNG or from the recording at 'at'. The rectangle is fractional."""
    x, y, width, height = rect
    if min(x, y) < 0 or min(width, height) <= 0 or x + width > 1.001 or y + height > 1.001:
        raise ValueError("video rectangle must fit inside the recorded frame")
    seek = ["-ss", str(at)] if at is not None else []
    filters = f"crop=iw*{width}:ih*{height}:iw*{x}:ih*{y},scale=100:100,format=rgb24"
    frame = subprocess.run(
        [ffmpeg, "-nostdin", "-v", "error", "-filter_threads", "1", *seek, "-i", str(source),
         "-vf", filters, "-frames:v", "1", "-threads", "1", "-f", "rawvideo", "-"],
        check=True, capture_output=True, timeout=8,
    ).stdout
    if len(frame) != 30000:
        raise ValueError("no complete frame at the requested time")

    def sample(x, y):
        offsets = [3 * (row * 100 + col) for row in range(y - 1, y + 2)
                   for col in range(x - 1, x + 2)]
        return [sum(frame[i + channel] for i in offsets) / len(offsets) for channel in range(3)]

    for (x, y), color in zip(CORNERS, COLORS):
        rgb = bytes.fromhex(color[1:])
        if any(abs(a - b) > 55 for a, b in zip(sample(x, y), rgb)):
            return False, None
    number = 0
    for bit, x in enumerate(BIT_CENTERS):
        pixel = sample(x, 61)
        if min(pixel) > 185:
            number |= 1 << bit
        elif max(pixel) > 65:
            return True, None
    return True, number
