#!/usr/bin/env python3
"""Run the pinned chatd probe with its matching release mediad."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

PROOF = "video-received-proof: room share+camera painted; direct share+camera painted; counters advance; cleared on stop"
LEGS = ("room-Share", "room-Camera", "direct-Share", "direct-Camera")


def record(out, evidence, passed):
    """Encode the received JPEGs as a playback of the probe, at ten samples per second."""
    ffmpeg = os.environ.get("E2E_FFMPEG", "ffmpeg")
    argv = [ffmpeg, "-nostdin", "-v", "error", "-y", "-filter_threads", "1"]
    steps = []
    segments = []
    index = 0
    for leg in LEGS:
        frames = sorted((evidence / leg).glob("*.jpg"))
        if not frames:
            continue
        steps.append(dict(t=index / 10, step=leg + " received JPEG samples", ok=len(frames) >= 11))
        index += len(frames)
        # The camera and share can paint at different sizes. Encode each leg before joining
        # them so the video encoder's dimensions and timestamps remain stable.
        name = leg + ".mp4"
        subprocess.run([*argv, "-framerate", "10", "-i", str(evidence / leg / "%04d.jpg"),
                        "-vf", "scale=640:384,setsar=1", "-c:v", "libx264", "-pix_fmt", "yuv420p",
                        str(evidence / name)], check=True, timeout=60)
        segments.append(f"file {name}\n")
    if index:
        listing = evidence / "legs.txt"
        listing.write_text("".join(segments), encoding="ascii")
        argv += ["-f", "concat", "-i", str(listing), "-c", "copy"]
    else:
        argv += ["-f", "lavfi", "-i", "color=black:s=640x384:r=10:d=2", "-c:v", "libx264", "-pix_fmt", "yuv420p"]
    steps.append(dict(t=max(0, index - 1) / 10, step="room and direct video probe", ok=passed))
    subprocess.run([*argv, str(out / "recording.mp4")], check=True, timeout=60)
    (out / "steps.jsonl").write_text("".join(json.dumps(s) + "\n" for s in steps), encoding="utf-8")

def main():
    root = Path(os.environ.get("E2E_SRC_DIR", Path(__file__).resolve().parents[3]))
    fixture = Path(os.environ.get("TINY_DIRECT_CALL_FIXTURE") or
                   Path.home() / ".config/tiny-lab/direct-call-fixture.json")
    if not fixture.is_file():
        sys.exit("TINY_DIRECT_CALL_FIXTURE must name an existing private fixture JSON")
    if not (root / "Cargo.toml").is_file():
        sys.exit("E2E_SRC_DIR must name the source checkout for the build being tested")
    env = dict(os.environ, TINY_DIRECT_CALL_FIXTURE=str(fixture.resolve()))
    if "E2E_APP_DIR" in env and "TINYCLIENT_TEST_MEDIAD" not in env:
        binaries = Path(env["E2E_APP_DIR"])
        if sys.platform == "darwin":
            binaries /= "Contents/MacOS"
        env["TINYCLIENT_TEST_MEDIAD"] = str(binaries.resolve() / ("mediad.exe" if os.name == "nt" else "mediad"))
    command = [os.environ.get("CARGO", "cargo"), "test", "--release", "-p", "chatd",
               "--no-default-features", "--lib", "real_video_round_trip", "--", "--ignored", "--nocapture"]
    with tempfile.TemporaryDirectory(prefix="tiny-received-") as evidence:
        if "E2E_OUT" in env:
            env["TINY_VIDEO_EVIDENCE"] = evidence
        result = subprocess.run(command, cwd=root, env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
        print(result.stdout, end="")
        passed = result.returncode == 0 and PROOF in result.stdout and "test result: ok. 1 passed" in result.stdout
        if "E2E_OUT" in env and "test result:" in result.stdout:
            record(Path(env["E2E_OUT"]), Path(evidence), passed)
        return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
