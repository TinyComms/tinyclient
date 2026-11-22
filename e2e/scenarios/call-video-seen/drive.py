#!/usr/bin/env python3
"""Drive two release windows through mouse input and the viewer's pixels."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time

from pixels import read_card

HERE = Path(__file__).resolve().parent


def command(*argv):
    return subprocess.run(list(map(str, argv)), check=True, capture_output=True,
                          text=True, timeout=12).stdout


def launch(argv, **kwargs):
    if os.name == "nt":
        kwargs.setdefault("creationflags", subprocess.CREATE_NO_WINDOW)
    return subprocess.Popen(argv, **kwargs)


def stop(process):
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


class Desktop:
    def __init__(self, platform, plan, pid, out):
        self.platform, self.plan, self.pid, self.out = platform, plan, pid, out
        self.ffmpeg = os.environ.get("E2E_FFMPEG", "ffmpeg")

    def win(self, action, *args):
        command("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                self.plan["win_window"], "-ProcessId", self.pid, "-Action", action, *args)

    def mac(self, action, *args):
        log = self.out / "helper-output.txt"
        log.unlink(missing_ok=True)
        command("open", "-W", "-n", "--stdout", log, "--stderr", log, "-a",
                os.environ.get("E2E_HELPER") or self.plan["mac_helper"], "--args",
                action, "--pid", self.pid, *args)
        return log.read_text(encoding="utf-8")

    def click(self, name):
        x, y = self.plan["clicks"][name]
        if self.platform == "windows":
            self.win("click", "-X", x, "-Y", y)
        elif self.platform == "macos":
            if "ok click" not in self.mac("click", "--x", x, "--y", y):
                raise RuntimeError(f"macOS click failed for {name}")
        else:
            command(sys.executable, self.plan["x11input"], "click", x, y)

    def shot(self, path):
        if self.platform == "windows":
            self.win("rect")
            command(self.ffmpeg, "-nostdin", "-v", "error", "-y", "-f", "gdigrab",
                    "-i", "desktop", "-frames:v", "1", path)
        elif self.platform == "macos":
            lines = self.mac("windows").splitlines()
            window = json.loads(lines[0])["id"]
            command("screencapture", "-x", "-o", "-l", window, path)
        else:
            command(self.ffmpeg, "-nostdin", "-v", "error", "-y", "-f", "x11grab",
                    "-video_size", self.plan.get("display_size", "1280x800"), "-i",
                    os.environ["DISPLAY"], "-frames:v", "1", path)


def sender(desktop):
    """Line protocol only coordinates actions between the two real desktops."""
    card = None
    print("ready", flush=True)
    try:
        for line in sys.stdin:
            action = line.strip()
            if action == "done":
                return
            if action == "start":
                desktop.click("share")
                time.sleep(.5)
                desktop.click("first_screen")
                time.sleep(.5)
                card = launch([sys.executable, str(HERE / "test-card.py")])
            elif action == "stop":
                if card is None or card.poll() is not None:
                    raise RuntimeError("sender's test card did not stay open")
                stop(card)
                desktop.click("stop_share")
                # Restore the card while B checks. Otherwise a working stream that shows
                # A's desktop after the card closes could be mistaken for a stopped share.
                card = launch([sys.executable, str(HERE / "test-card.py")])
                time.sleep(.4)
                if card.poll() is not None:
                    raise RuntimeError("sender could not restore the test card")
            elif action == "close_card":
                stop(card)
                card = None
            else:
                if action not in {"conversation", "call", "leave", "room", "join"}:
                    raise ValueError("unknown sender action")
                desktop.click(action)
            print(action, flush=True)
    finally:
        if card is not None:
            stop(card)


def viewer(desktop, plan):
    out = desktop.out
    start = float(os.environ.get("E2E_T0", time.time()))
    rects = plan["video_rects"]
    replies = queue.Queue()
    with launch(plan["sender_command"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                text=True, bufsize=1) as peer:
        def receive():
            for line in peer.stdout:
                replies.put(line.strip())
            replies.put(None)

        threading.Thread(target=receive, daemon=True).start()

        def expect(reply):
            if replies.get(timeout=15) != reply:
                raise RuntimeError(f"sender did not complete {reply}")

        def tell(action):
            peer.stdin.write(action + "\n")
            peer.stdin.flush()
            expect(action)

        with (out / "steps.jsonl").open("w", encoding="utf-8") as log:
            def step(name, ok, shot=None, **metadata):
                item = dict(t=round(time.time() - start, 2), step=name, ok=ok, **metadata)
                if shot is not None:
                    item["shot"] = shot.name
                log.write(json.dumps(item) + "\n")
                log.flush()
                if not ok:
                    raise RuntimeError(name)

            def sample_until(path, rect, seconds, matches, stable=0):
                deadline = time.monotonic() + seconds
                matched_since = None
                while time.monotonic() < deadline:
                    at = time.time() - start
                    desktop.shot(path)
                    visible, number = read_card(desktop.ffmpeg, path, rect)
                    now = time.monotonic()
                    if matches(visible, number):
                        if matched_since is None:
                            matched_since = now
                        if now <= deadline and now - matched_since >= stable:
                            return True, number, at
                    else:
                        matched_since = None
                    time.sleep(.1)
                return False, None, None

            try:
                expect("ready")
                tell("conversation")
                tell("call")
                desktop.click("accept")
                time.sleep(2)
                shots = (5, 8)
                for mode, count in zip(("direct", "room"), shots):
                    rect = rects[mode]
                    if mode == "room":
                        tell("room")
                        tell("join")
                        desktop.click("room")
                        desktop.click("join")
                        time.sleep(2)
                    tell("start")
                    if mode == "room":
                        desktop.click("watch")
                    path = out / f"{count:02}-seen.png"
                    ok, number, at = sample_until(path, rect, 10, lambda visible, number: visible and number is not None)
                    step("B sees A's screen" if mode == "direct" else "B sees A's room share",
                         ok, path, video_card=dict(at=at, rect=rect, number=number))
                    time.sleep(2)
                    path = out / f"{count + 1:02}-moving.png"
                    ok, later, at = sample_until(path, rect, 3,
                                                 lambda visible, later: visible and later is not None and later != number)
                    step(f"{mode} share number changes", ok, path,
                         video_card=dict(at=at, rect=rect, number=later))
                    stop_started = time.monotonic()
                    tell("stop")
                    path = out / f"{count + 2:02}-stopped.png"
                    remaining = max(0, 5 - (time.monotonic() - stop_started))
                    ok, _, at = sample_until(path, rect, remaining, lambda visible, _: not visible, stable=1)
                    step(f"{mode} share clears", ok, path,
                         video_card=dict(at=at, rect=rect, number=None))
                    tell("close_card")
                    desktop.click("leave")
                    tell("leave")
                peer.stdin.write("done\n")
                peer.stdin.flush()
                if peer.wait(timeout=10) != 0:
                    raise RuntimeError("sender driver failed")
            finally:
                stop(peer)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=["windows", "linux", "macos"])
    parser.add_argument("--sender", action="store_true")
    parser.add_argument("--plan", default=os.environ.get("E2E_CALL_VIDEO_PLAN"))
    args = parser.parse_args()
    plan_path = Path(args.plan or Path.home() / f".config/tiny-lab/call-video-{args.platform}.json")
    if not plan_path.is_file():
        parser.error("set E2E_CALL_VIDEO_PLAN to a private two-host setup file")
    plan = json.loads(plan_path.read_text(encoding="utf-8"))
    app_dir = Path(os.environ.get("E2E_APP_DIR") or plan["app_dir"])
    out = Path(os.environ.get("E2E_OUT") or plan["out_dir"])
    out.mkdir(parents=True, exist_ok=True)
    binaries = app_dir.resolve()
    if args.platform == "macos":
        binaries /= "Contents/MacOS"
    suffix = ".exe" if args.platform == "windows" else ""
    env = dict(os.environ)
    env.pop("TINYCLIENT_DEBUG_JOIN", None)
    env.pop("TINYCLIENT_DIR", None)
    if args.platform == "macos":
        profile = Path(env["HOME"]) / "Library/Application Support/tinyclient"
    elif args.platform == "windows":
        profile = Path(env["APPDATA"]) / "tinyclient"
    else:
        profile = Path(env.get("XDG_DATA_HOME") or Path(env["HOME"]) / ".local/share") / "tinyclient"
    if profile.resolve() != Path(plan["profile"]).resolve(strict=True):
        raise RuntimeError("run the driver inside the signed-in fixture OS user's desktop")
    with contextlib.ExitStack() as stack:
        app_log = stack.enter_context((out / "app.log").open("w", encoding="utf-8"))
        chatd = launch([str(binaries / ("chatd" + suffix))], env=env, stdout=app_log, stderr=app_log)
        stack.callback(stop, chatd)
        app = launch([str(binaries / ("tinyclient" + suffix))], env=env, stdout=app_log, stderr=app_log)
        stack.callback(stop, app)
        (out / "app.pid").write_text(str(app.pid), encoding="ascii")
        desktop = Desktop(args.platform, plan, app.pid, out)
        deadline = time.monotonic() + 15
        while True:
            try:
                desktop.shot(out / "ready.png")
                break
            except (subprocess.CalledProcessError, IndexError, json.JSONDecodeError):
                if app.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("tinyclient release window did not open") from None
                time.sleep(.25)
        (sender if args.sender else lambda d: viewer(d, plan))(desktop)


if __name__ == "__main__":
    main()
