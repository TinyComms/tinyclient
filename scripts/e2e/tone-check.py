"""What a mediad sink file (TINYCLIENT_MEDIAD_SINK: s16le, 48 kHz, stereo) heard.

    python -I tone-check.py <played.pcm> [<played.pcm> ...]

For each file: its length, RMS, how much of it is sound, the strongest frequency, and the
energy at 440 Hz (the desktop's test tone) and 660 Hz (the phone's), each against the median
of the spectrum. Only the standard library: a Goertzel filter per probe, a coarse DFT scan.
"""
import math
import struct
import sys

RATE = 48000


def goertzel(xs, hz):
    k = 2 * math.cos(2 * math.pi * hz / RATE)
    s1 = s2 = 0.0
    for x in xs:
        s1, s2 = x + k * s1 - s2, s1
    return math.sqrt(max(s1 * s1 + s2 * s2 - k * s1 * s2, 0.0)) / len(xs)


def check(path):
    data = open(path, "rb").read()
    n = len(data) // 4
    if n == 0:
        print(f"{path}: empty")
        return
    frames = struct.unpack(f"<{n * 2}h", data[: n * 4])
    mono = [(frames[2 * i] + frames[2 * i + 1]) / 65536.0 for i in range(n)]
    seconds = n / RATE
    rms = math.sqrt(sum(x * x for x in mono) / n)
    # 100 ms windows with sound in them.
    win = RATE // 10
    loud = sum(1 for i in range(0, n - win, win) if math.sqrt(sum(x * x for x in mono[i : i + win]) / win) > 0.005)
    total = max((n - win) // win, 1)
    # The loudest second, for the spectrum.
    best, best_at = -1.0, 0
    for i in range(0, max(n - RATE, 1), RATE // 2):
        e = sum(x * x for x in mono[i : i + RATE : 8])
        if e > best:
            best, best_at = e, i
    chunk = mono[best_at : best_at + RATE // 4]
    scan = {hz: goertzel(chunk, hz) for hz in range(100, 2000, 20)}
    peak = max(scan, key=scan.get)
    median = sorted(scan.values())[len(scan) // 2] or 1e-12
    t440, t660 = goertzel(chunk, 440), goertzel(chunk, 660)
    print(
        f"{path}: {seconds:.1f} s, rms {rms:.4f}, sound in {loud}/{total} 100 ms windows, "
        f"peak {peak} Hz, 440 Hz {t440 / median:.0f}x median, 660 Hz {t660 / median:.0f}x median "
        f"(loudest 250 ms at {best_at / RATE:.1f} s)"
    )


for p in sys.argv[1:]:
    try:
        check(p)
    except OSError as e:
        print(f"{p}: {e}")
