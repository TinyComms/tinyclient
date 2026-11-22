#!/usr/bin/env python3
"""Run the packaged mediad's Settings microphone test with its synthetic microphone."""
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time


def meter_frames(stream, frames):
    try:
        while header := stream.read(4):
            size = int.from_bytes(header, "big")
            if len(header) != 4 or not 0 < size <= 1024 * 1024:
                raise ValueError("invalid mediad frame size")
            frames.put(json.loads(stream.read(size)))
    except (ValueError, OSError) as exc:
        frames.put(exc)
    finally:
        frames.put(None)


def check(exe, scratch):
    env = dict(os.environ, TINYCLIENT_DIR=scratch, TINYCLIENT_MEDIAD_AUDIO="tone")
    with subprocess.Popen([exe, "test"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE) as process:
        frames = queue.Queue()
        reader = threading.Thread(target=meter_frames, args=(process.stdout, frames), daemon=True)
        reader.start()
        try:
            request = b'{"media":"meter","on":true}'
            process.stdin.write(len(request).to_bytes(4, "big") + request)
            process.stdin.flush()
            limit = time.monotonic() + 12
            samples = 0
            while time.monotonic() < limit:
                reply = frames.get(timeout=max(.01, limit - time.monotonic()))
                if not isinstance(reply, dict) or reply.get("media") in {"failed", "refused"}:
                    raise RuntimeError(f"mediad microphone test ended: {reply}")
                if reply.get("media") == "levels" and reply["input"] > 0:
                    samples += 1
                    if samples == 3:
                        break
            if samples < 3:
                raise RuntimeError("mediad did not measure three nonzero microphone levels")
            process.stdin.close()
            if process.wait(timeout=5) != 0:
                raise RuntimeError("mediad microphone test did not exit cleanly")
            print("packaged mediad measured three nonzero microphone levels")
        finally:
            if process.poll() is None:
                process.kill()
            reader.join(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: microphone-smoke.py PATH_TO_MEDIAD")
    with tempfile.TemporaryDirectory(prefix="tiny-mic-") as scratch:
        try:
            check(sys.argv[1], scratch)
        except queue.Empty:
            sys.exit("mediad microphone test timed out")
