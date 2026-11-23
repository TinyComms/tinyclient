#!/usr/bin/env python3
"""Run the real release mediad on both ends of a call, first automatic then OpenH264."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

LEGS = ("room-Share", "room-Camera", "direct-Share", "direct-Camera")


def record(out, evidence, passed, legs):
    """Encode the received JPEGs as a playback of the probe, at ten samples per second."""
    ffmpeg = os.environ.get("E2E_FFMPEG", "ffmpeg")
    argv = [ffmpeg, "-nostdin", "-v", "error", "-y", "-filter_threads", "1"]
    steps = []
    segments = []
    index = 0
    for folder, label in legs:
        frames = sorted(folder.glob("*.jpg"))
        if not frames:
            continue
        steps.append(dict(t=index / 10, step=label, ok=len(frames) >= 11))
        index += len(frames)
        # The camera and share can paint at different sizes. Encode each leg before joining
        # them so the video encoder's dimensions and timestamps remain stable.
        name = f"segment-{len(segments)}.mp4"
        subprocess.run([*argv, "-framerate", "10", "-i", str(folder / "%04d.jpg"),
                        "-vf", "scale=640:384,setsar=1", "-c:v", "libx264", "-pix_fmt", "yuv420p",
                        str(evidence / name)], check=True, timeout=60)
        segments.append(f"file {name}\n")
    if index:
        listing = evidence / "legs.txt"
        listing.write_text("".join(segments), encoding="ascii")
        argv += ["-f", "concat", "-i", str(listing), "-c", "copy"]
    else:
        argv += ["-f", "lavfi", "-i", "color=black:s=640x384:r=10:d=2", "-c:v", "libx264", "-pix_fmt", "yuv420p"]
    steps.append(dict(t=max(0, index - 1) / 10, step="Automatic and OpenH264 call video", ok=passed))
    subprocess.run([*argv, str(out / "recording.mp4")], check=True, timeout=60)
    (out / "steps.jsonl").write_text("".join(json.dumps(s) + "\n" for s in steps), encoding="utf-8")

def probe_command():
    root = Path(os.environ.get("E2E_SRC_DIR", Path.cwd()))
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
    return root, env, command


def main():
    root, env, command = probe_command()
    launcher = env.get("E2E_APP_DIR") or env.get("TINYCLIENT_TEST_MEDIAD")
    if not launcher:
        sys.exit("Set E2E_APP_DIR or TINYCLIENT_TEST_MEDIAD to the release build.")
    with tempfile.TemporaryDirectory(prefix="gpu-codecs-wave18-") as scratch:
        evidence = Path(scratch)
        legs = []
        passed = True
        for mode in ("auto", "software"):
            directory = evidence / mode
            directory.mkdir()
            settings = dict(env, TINY_GPU_CODECS_PROOF=mode, TINY_VIDEO_EVIDENCE=str(directory))
            if mode == "software":
                settings["TINYCLIENT_MEDIAD_VIDEO_CODEC"] = "software"
            else:
                settings.pop("TINYCLIENT_MEDIAD_VIDEO_CODEC", None)
            run = subprocess.run(command, cwd=root, env=settings, stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
            print(run.stdout, end="")
            for leg in LEGS:
                paths = [directory / f"{leg}-{direction}.txt" for direction in ("encoder", "decoder")]
                encoder, decoder = (path.read_text(encoding="utf-8") if path.is_file() else "" for path in paths)
                if not encoder or not decoder:
                    passed = False
                legs.append((directory / leg, f"{mode} {leg} | encoder {encoder or 'missing'} | decoder {decoder or 'missing'}"))
            passed = passed and run.returncode == 0
            if not passed:
                break
        if "E2E_OUT" in env:
            record(Path(env["E2E_OUT"]), evidence, passed, legs)
        return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
