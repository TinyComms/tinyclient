"""wingrab.ps1's frames -> one captioned clip, then the lab's own recording checks.

  python lib/frames2mp4.py <frames-dir> <steps.jsonl> <out.mp4> <t0 unix> "<header>"

Each frame lasts until the next one (ffmpeg concat with durations from frames.txt), so the clip's
time is wall time from t0, the same clock steps.jsonl uses. Captioning (SHA/run and the current step
burned in) and validation are e2e/lab/common.py's `caption` and `validate`, unchanged.
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..", "..", "..", "lab")))
import common  # noqa: E402


def main():
    frames, steps_path, out, t0, header = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
    idx = [l.split() for l in open(os.path.join(frames, "frames.txt")) if l.strip()]
    if not idx:
        print("no frames")
        return 1
    lst = os.path.join(frames, "concat.txt")
    with open(lst, "w") as f:
        # pad the start: from t0 to the first frame, show the first frame
        first = float(idx[0][1])
        for i, (name, t) in enumerate(idx):
            t = float(t)
            nxt = float(idx[i + 1][1]) if i + 1 < len(idx) else t + 1.0
            dur = (nxt - t) + ((t - t0) if i == 0 and t > t0 else 0)
            f.write(f"file '{name}'\nduration {max(dur, 0.05):.3f}\n")
        f.write(f"file '{idx[-1][0]}'\n")
    raw = out.replace(".mp4", "-raw.mp4")
    r = subprocess.run([common.tool("ffmpeg"), "-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i", lst,
                        "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p", raw],
                       capture_output=True, text=True)
    if r.returncode:
        print("ffmpeg:", r.stderr[-400:])
        return 1
    steps = [json.loads(l) for l in open(steps_path, encoding="utf-8") if l.strip()] if os.path.exists(steps_path) else []
    seconds = float(idx[-1][1]) - t0 + 1.0
    common.caption(raw, out, header, steps, seconds)
    problems = common.validate(out, seconds, steps, {"max_still_seconds": 120})
    with open(out + ".check.json", "w") as f:
        json.dump({"problems": problems, "frames": len(idx), "seconds": round(seconds, 1)}, f, indent=1)
    print(f"recording {out}: {len(idx)} frames, {seconds:.0f}s, problems: {problems or 'none'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
