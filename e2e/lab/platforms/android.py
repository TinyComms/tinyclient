"""Android lab adapter: builds the app for the emulator, films the emulator's own screen with
`adb shell screenrecord`, and runs the scenario's driver while it films.

It runs on zeppelin itself (hosts.json group "zeppelin"). Nothing here touches this PC's screen
or any adb device but `host["serial"]`.

The app under test is the phone proof build (`build-android.ps1 -Demo`, cargo feature `demo`): the
made-up chatd answers from launch, so the app shows invented people with no account, no server and
no network. That is not byte-for-byte the shipped build (the shipped one has no demo chatd); it is
the only way the phone app shows data with nothing signed in. E2E_SABOTAGE=1 builds the same app
WITHOUT the feature, so it starts on the first-run screen and the driver's inbox step fails.

Caches (all under %LOCALAPPDATA%\\tiny-e2e\\android, or $E2E_ANDROID_CACHE):
  src/      the source of the last build; a new SHA only rewrites the files that differ, so cargo
            and Gradle see unchanged crates as unchanged
  target/   the cargo target directory (shared by the demo and the sabotage build)
Standard library only.
"""
import json
import os
import shutil
import subprocess
import sys
import tarfile
import traceback
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import common  # noqa: E402

PACKAGE = "dev.tinyclient.app"
SEGMENT_SECONDS = int(os.environ.get("E2E_ANDROID_SEGMENT", "170"))  # screenrecord stops by itself at 180
RECORD_SIZE = os.environ.get("E2E_ANDROID_SIZE", "540x1200")
RECORD_BITRATE = os.environ.get("E2E_ANDROID_BITRATE", "4000000")
BOOT_SECONDS = 300
DRIVER_SECONDS = 300
PERMISSIONS = ("android.permission.CAMERA", "android.permission.RECORD_AUDIO", "android.permission.POST_NOTIFICATIONS")


def home():
    return os.path.expanduser("~")


def cache_dir():
    root = os.environ.get("E2E_ANDROID_CACHE") or os.path.join(os.environ.get("LOCALAPPDATA") or os.path.join(home(), ".cache"), "tiny-e2e", "android")
    os.makedirs(root, exist_ok=True)
    return root


def sdk_dir():
    for candidate in (os.environ.get("ANDROID_HOME"), os.environ.get("ANDROID_SDK_ROOT"),
                      os.path.join(os.environ.get("LOCALAPPDATA") or "", "Android", "Sdk")):
        if candidate and os.path.isdir(candidate):
            return candidate
    raise RuntimeError("no Android SDK (set ANDROID_HOME)")


def find_adb():
    for candidate in (os.environ.get("E2E_ADB"), os.path.join(home(), ".local", "bin", "adb.exe"), shutil.which("adb"),
                      os.path.join(sdk_dir(), "platform-tools", "adb.exe")):
        if candidate and os.path.isfile(candidate):
            return candidate
    raise RuntimeError("adb not found (set E2E_ADB)")


def find_emulator():
    for candidate in (os.environ.get("E2E_EMULATOR"), os.path.join(sdk_dir(), "emulator", "emulator.exe"),
                      os.path.join(home(), ".cargo", "bin", "emulator.exe"), shutil.which("emulator")):
        if candidate and os.path.isfile(candidate):
            return candidate
    raise RuntimeError("emulator not found (set E2E_EMULATOR)")


class Device:
    """adb, pinned to one serial."""

    def __init__(self, adb, serial):
        self.adb, self.serial = adb, serial

    def run(self, *args, timeout=60, check=False):
        try:
            return subprocess.run([self.adb, "-s", self.serial, *args], capture_output=True, text=True, timeout=timeout, check=check)
        except subprocess.TimeoutExpired:
            return subprocess.CompletedProcess(args, 124, "", "adb timed out")

    def sh(self, command, timeout=60):
        return self.run("shell", command, timeout=timeout).stdout.strip()

    def state(self):
        out = subprocess.run([self.adb, "devices"], capture_output=True, text=True, timeout=30).stdout
        for line in out.splitlines()[1:]:
            parts = line.split()
            if len(parts) >= 2 and parts[0] == self.serial:
                return parts[1]
        return None

    def booted(self):
        return self.state() == "device" and self.sh("getprop sys.boot_completed", 15) == "1"


# ---------------------------------------------------------------------------------------- source

LOOPBACK_MOD = "crates/tinyclient/src/mobile/android/mod.rs"
LOOPBACK_SRC = "crates/tinyclient/src/mobile/android/loopback.rs"
PHONE_PICTURES = "crates/mediad/src/picture/phone.rs"


def read_member(tar_path, name):
    with tarfile.open(tar_path) as tar:
        try:
            return tar.extractfile(name).read().decode("utf-8", "replace")
        except KeyError:
            return None


def drop_loopback(data):
    """mod.rs without the demo call's camera loopback (`mod loopback;` and `loopback::register();`)."""
    text = data.decode("utf-8")
    for line in ('#[cfg(feature = "demo")]\nmod loopback;\n', '    #[cfg(feature = "demo")]\n    loopback::register();\n'):
        text = text.replace(line, "").replace(line.replace("\n", "\r\n"), "")
    return text.encode("utf-8")


def source_fixes(tar_path, log):
    """{path: transform} for known breakage in the tree that has nothing to do with the scenario.

    The phone `demo` build (the only way the phone app shows invented data with no account) does not
    compile when its camera loopback (`mobile/android/loopback.rs`, the demo call) still uses
    `mediad::picture::phone::Pictures`, which mediad no longer has (it is `Outgoing`/`Incoming`
    now). The loopback only feeds the demo's call screen, which `smoke` never opens, so the lab
    builds the demo without it and says so. Once loopback.rs is fixed this does nothing."""
    loopback = read_member(tar_path, LOOPBACK_SRC)
    pictures = read_member(tar_path, PHONE_PICTURES)
    if loopback and "phone::Pictures" in loopback and pictures is not None and "pub struct Pictures" not in pictures:
        log("source: loopback.rs uses mediad::picture::phone::Pictures, which no longer exists; building the demo without the loopback")
        return {LOOPBACK_MOD: drop_loopback}
    return {}


def sync_tree(tar_path, dest, manifest_path, log, fixes=None):
    """Extract the tar into `dest`, rewriting only files whose bytes differ, and removing files the
    previous extraction made that this tar no longer has. An untouched file keeps its mtime, which
    is what lets cargo and Gradle skip what did not change. Build outputs in `dest` are untouched."""
    fixes = fixes or {}
    old = set()
    if os.path.isfile(manifest_path):
        with open(manifest_path, encoding="utf-8") as f:
            old = set(json.load(f))
    new, written = set(), 0
    with tarfile.open(tar_path) as tar:
        for member in tar:
            if member.type == tarfile.XHDTYPE or member.type == tarfile.XGLTYPE or not member.isfile():
                if member.isdir():
                    os.makedirs(os.path.join(dest, member.name), exist_ok=True)
                continue
            target = os.path.join(dest, member.name)
            data = tar.extractfile(member).read()
            if member.name in fixes:
                data = fixes[member.name](data)
            new.add(member.name)
            if os.path.isfile(target) and os.path.getsize(target) == len(data):
                with open(target, "rb") as f:
                    if f.read() == data:
                        continue
            os.makedirs(os.path.dirname(target), exist_ok=True)
            with open(target, "wb") as f:
                f.write(data)
            written += 1
    removed = 0
    for rel in old - new:
        try:
            os.remove(os.path.join(dest, rel))
            removed += 1
        except OSError:
            pass
    with open(manifest_path, "w", encoding="utf-8") as f:
        json.dump(sorted(new), f)
    log("source: %d files, %d rewritten, %d removed" % (len(new), written, removed))


def stream(cmd, log_path, env, cwd=None, timeout=5400):
    """Run `cmd` with its output going to a file. Returns the exit code (124 on timeout)."""
    with open(log_path, "a", encoding="utf-8", errors="replace") as out:
        out.write("$ %s\n" % " ".join(cmd))
        out.flush()
        try:
            return subprocess.run(cmd, stdout=out, stderr=subprocess.STDOUT, env=env, cwd=cwd, timeout=timeout).returncode
        except subprocess.TimeoutExpired:
            out.write("\ntimed out after %ds\n" % timeout)
            return 124


def tail(path, n=12):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return " | ".join(line.strip() for line in f.read().splitlines()[-n:] if line.strip())[-600:]
    except OSError:
        return ""


def build(ctx, sabotage, build_log):
    """The debug APK for x86_64, as scripts/e2e/local-tiny-call.ps1 -BuildApk makes it. Returns its path."""
    cache = cache_dir()
    src = os.path.join(cache, "src")
    os.makedirs(src, exist_ok=True)
    t0 = time.time()
    sync_tree(ctx.source_tar, src, os.path.join(cache, "manifest.json"), ctx.log, source_fixes(ctx.source_tar, ctx.log))
    sdk = sdk_dir()
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = os.path.join(cache, "target")
    env["ANDROID_HOME"] = sdk
    env["PATH"] = os.pathsep.join([os.path.join(home(), ".cargo", "bin"), env.get("PATH", "")])
    manual = sorted(p for p in (os.path.join(home(), ".gradle", "dists-manual", d, "bin") for d in
                                (os.listdir(os.path.join(home(), ".gradle", "dists-manual")) if os.path.isdir(os.path.join(home(), ".gradle", "dists-manual")) else []))
                    if os.path.isdir(p))
    if manual and not shutil.which("gradle"):
        env["PATH"] = manual[-1] + os.pathsep + env["PATH"]
    powershell = shutil.which("pwsh") or shutil.which("powershell")
    script = os.path.join(src, "scripts", "mobile", "build-android.ps1")
    cmd = [powershell, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script, "-Abi", "x86_64", "-SkipGradle"]
    if not sabotage:
        cmd.append("-Demo")
    ctx.log("build: cargo ndk (%s)" % ("no demo chatd: sabotage" if sabotage else "demo chatd"))
    if stream(cmd, build_log, env) != 0:
        raise RuntimeError("cargo ndk build failed: " + tail(build_log))
    ctx.log("build: cargo done in %.0fs" % (time.time() - t0))
    # The debug .so is over 1 GB with its debug info; stripped it installs in seconds, and it keeps
    # debug_assertions (the debug launch).
    so = os.path.join(src, "mobile", "android", "app", "src", "main", "jniLibs", "x86_64", "libtinyclient.so")
    strips = sorted(p for p in (os.path.join(dp, f) for dp, _, fs in os.walk(os.path.join(sdk, "ndk")) for f in fs) if os.path.basename(p) == "llvm-strip.exe")
    if not strips:
        raise RuntimeError("llvm-strip not found in the NDK")
    if stream([strips[-1], "--strip-debug", so], build_log, env) != 0:
        raise RuntimeError("llvm-strip failed")
    gradle = shutil.which("gradle", path=env["PATH"])
    if not gradle:
        raise RuntimeError("gradle is not on PATH or in ~/.gradle/dists-manual")
    t1 = time.time()
    # --no-daemon: a daemon would outlive the run, and "nothing stray is left" is part of the contract.
    if stream([gradle, "--no-daemon", "-q", ":app:assembleDebug", "--console=plain"], build_log, env, cwd=os.path.join(src, "mobile", "android")) != 0:
        raise RuntimeError("gradle assembleDebug failed: " + tail(build_log))
    ctx.log("build: gradle done in %.0fs" % (time.time() - t1))
    apk = os.path.join(src, "mobile", "android", "app", "build", "outputs", "apk", "debug", "app-debug.apk")
    if not os.path.isfile(apk):
        raise RuntimeError("gradle reported success but there is no " + apk)
    return apk


# ------------------------------------------------------------------------------------- emulator

class Emulator:
    """Boots Pixel_7 headless unless the serial is already up, and shuts down only what it started."""

    def __init__(self, ctx, dev, log_path):
        self.ctx, self.dev, self.log_path = ctx, dev, log_path
        self.proc = None
        self.started = False

    def start(self):
        state = self.dev.state()
        if state == "device":
            self.ctx.log("emulator: %s is already running; leaving it running afterwards" % self.dev.serial)
            return
        if state is not None:
            raise RuntimeError("%s is %s: not mine to restart" % (self.dev.serial, state))
        port = self.dev.serial.split("-")[-1]
        cmd = [find_emulator(), "-avd", self.ctx.host["avd"], "-port", port, "-no-window", "-no-audio",
               "-no-snapshot-save", "-no-boot-anim", "-no-metrics", "-gpu", os.environ.get("E2E_ANDROID_GPU", "host")]
        self.ctx.log("emulator: starting headless: " + " ".join(cmd[1:]))
        log = open(self.log_path, "w", encoding="utf-8")
        flags = getattr(subprocess, "CREATE_NO_WINDOW", 0) | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
        self.proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, creationflags=flags)
        self.started = True

    def wait_booted(self):
        deadline = time.time() + BOOT_SECONDS
        began = time.time()
        while time.time() < deadline:
            if self.proc is not None and self.proc.poll() is not None:
                raise RuntimeError("the emulator exited with %s while booting: %s" % (self.proc.returncode, tail(self.log_path, 4)))
            if self.dev.booted():
                self.dev.sh("input keyevent 82")
                self.ctx.log("emulator: booted in %.0fs" % (time.time() - began))
                return
            time.sleep(2)
        raise RuntimeError("the emulator was not booted after %ds" % BOOT_SECONDS)

    def stop(self):
        if not self.started:
            return
        self.ctx.log("emulator: shutting down the one this run started")
        self.dev.run("emu", "kill", timeout=30)
        if self.proc is not None:
            try:
                self.proc.wait(timeout=45)
            except subprocess.TimeoutExpired:
                subprocess.run(["taskkill", "/F", "/T", "/PID", str(self.proc.pid)], capture_output=True)
                try:
                    self.proc.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    pass
        # emulator.exe hands over to a qemu process; make sure none with our AVD is left.
        deadline = time.time() + 30
        while time.time() < deadline and self.dev.state() is not None:
            time.sleep(1)


# -------------------------------------------------------------------------------------- recorder

class Recorder:
    """Back-to-back `screenrecord` segments (it stops by itself at 180 s), pulled and joined at the end.

    The chain is one shell loop on the device, started with a single non-blocking `adb shell`. A
    Python thread running `subprocess.run` for each segment instead made every other process this
    one started wait until that segment ended (measured: a driver launched 170 s late on Windows)."""

    def __init__(self, ctx, dev, where):
        self.ctx, self.dev, self.where = ctx, dev, where
        self.proc = None
        self.t0 = None

    def start(self):
        self.dev.run("shell", "pkill -2 screenrecord; rm -f /sdcard/e2e-*.mp4 /sdcard/e2e-stop")
        loop = ("i=0; while [ ! -e /sdcard/e2e-stop ]; do screenrecord --size %s --bit-rate %s --time-limit %d /sdcard/e2e-$i.mp4; "
                "i=$((i+1)); done" % (RECORD_SIZE, RECORD_BITRATE, SEGMENT_SECONDS))
        self.proc = subprocess.Popen([self.dev.adb, "-s", self.dev.serial, "shell", loop],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.time() + 20
        while time.time() < deadline:
            if self.dev.sh("pidof screenrecord"):
                self.t0 = time.time()
                return self.t0
            time.sleep(0.2)
        raise RuntimeError("screenrecord did not start")

    def stop(self):
        """Ends the last segment cleanly (SIGINT lets screenrecord finish the file). Returns seconds since start."""
        self.dev.run("shell", "touch /sdcard/e2e-stop")
        deadline = time.time() + 30
        while self.proc.poll() is None and time.time() < deadline:
            self.dev.run("shell", "pkill -2 screenrecord", timeout=15)
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
        if self.proc.poll() is None:
            self.proc.kill()
        ended = time.time() - self.t0
        time.sleep(0.5)  # the muxer has closed the file
        return ended

    def collect(self, out_path, seconds):
        """Pull the segments, give each the length it was recorded for, and join them with ffmpeg's
        concat demuxer (stream copy). '' when there is nothing to join.

        screenrecord writes a frame only where the screen changed, and its container's duration is
        not to be trusted (measured: 17.6 s for a 10 s segment whose only frames were at 0 and 8.8 s).
        A still stretch at the end of a segment is missing, and a segment of a screen that never
        moved holds one frame or none. Either would pull every later step off the picture and leave
        the file shorter than the run, which the lab's check refuses. So each segment is re-cut at 10
        fps to SEGMENT_SECONDS (the last: the rest of the run), repeating its last frame; a segment
        with no frames of its own repeats its neighbour's. All of them are then one codec, so the
        join can copy."""
        ffmpeg = common.tool("ffmpeg")
        names = sorted((n for n in self.dev.sh("ls /sdcard").split() if n.startswith("e2e-") and n.endswith(".mp4")),
                       key=lambda n: int(n[4:-4]))
        parts = []  # (local path, has frames of its own)
        for n, name in enumerate(names):
            local = os.path.join(self.where, "segment-%03d.mp4" % n)
            self.dev.run("pull", "/sdcard/" + name, local, timeout=120)
            info = common.probe(local) if os.path.isfile(local) and os.path.getsize(local) > 0 else None
            parts.append((local, info is not None and info["seconds"] > 0.05, info is not None))
        self.dev.run("shell", "rm -f /sdcard/e2e-*.mp4 /sdcard/e2e-stop")
        self.ctx.log("recorder: %d segment(s): %s" % (len(parts), ["moving" if m else ("one frame" if f else "empty") for _, m, f in parts]))
        if not any(f for _, _, f in parts):
            return ""
        encode = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p"]
        fixed = []
        for n, (part, moving, framed) in enumerate(parts):
            target = SEGMENT_SECONDS if n < len(parts) - 1 else max(1.0, seconds - SEGMENT_SECONDS * (len(parts) - 1))
            out = os.path.join(self.where, "segment-%03d-fixed.mp4" % n)
            if moving:
                cmd = [ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-i", part, "-an", "-vf", "fps=10,tpad=stop_mode=clone:stop_duration=%.2f" % (target + 1),
                       "-t", "%.2f" % target, *encode, out]
            else:
                # One frame, or none: repeat a picture. Its own, else the last of the segment before, else the first after.
                before = [p for p, _, f in parts[:n] if f]
                after = [p for p, _, f in parts[n + 1:] if f]
                source, seek = (part, []) if framed else ((before[-1], ["-sseof", "-0.2"]) if before else (after[0], []))
                still = os.path.join(self.where, "still.png")
                common.run([ffmpeg, "-hide_banner", "-loglevel", "error", "-y", *seek, "-i", source, "-frames:v", "1", still])
                cmd = [ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-loop", "1", "-framerate", "10", "-i", still, "-t", "%.2f" % target, *encode, out]
            done = common.run(cmd, check=False)
            if done.returncode != 0 or not common.probe(out):
                raise RuntimeError("re-cutting segment %d failed: %s" % (n, done.stderr[-300:]))
            fixed.append(out)
        listing = os.path.join(self.where, "segments.txt")
        with open(listing, "w", encoding="utf-8") as f:
            for part in fixed:
                f.write("file '%s'\n" % part.replace("\\", "/"))
        done = common.run([ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0",
                           "-i", listing, "-c", "copy", out_path], check=False)
        if done.returncode != 0:
            raise RuntimeError("joining the recording failed: " + done.stderr[-300:])
        for path in [p for p, _, _ in parts] + fixed + [listing, os.path.join(self.where, "still.png")]:
            if os.path.exists(path):
                os.remove(path)
        return out_path


# ------------------------------------------------------------------------------------------ run

def driver_for(ctx):
    for name in ("drive.android.py", "drive.android.ps1"):
        path = os.path.join(ctx.scenario_dir, name)
        if os.path.isfile(path):
            return path
    return None


def run(ctx):
    out = ctx.out_dir
    os.makedirs(out, exist_ok=True)
    sabotage = os.environ.get("E2E_SABOTAGE") == "1"
    outcome = common.Outcome(passed=False)
    started = time.time()
    adb = find_adb()
    dev = Device(adb, ctx.host.get("serial", "emulator-5554"))
    build_log = os.path.join(out, "android-build.log")
    emu_log = os.path.join(out, "android-emulator.log")
    logcat_path = os.path.join(out, "android-logcat.txt")
    driver_log = os.path.join(out, "android-driver.log")
    steps_path = os.path.join(out, "steps.jsonl")
    for stale in (steps_path,):
        if os.path.exists(stale):
            os.remove(stale)
    outcome.logs = [build_log, emu_log, logcat_path, driver_log]
    driver = driver_for(ctx)
    if driver is None:
        outcome.note = "scenario %s has no drive.android.py or drive.android.ps1" % ctx.scenario["id"]
        return outcome

    emulator = Emulator(ctx, dev, emu_log)
    recorder = None
    try:
        # Boot while the build runs: the build is the long part, and a booted emulator is idle.
        emulator.start()
        apk = build(ctx, sabotage, build_log)
        outcome.binary_sha256 = common.sha256(apk)
        emulator.wait_booted()

        ctx.log("install: %s (%.0f MB)" % (os.path.basename(apk), os.path.getsize(apk) / 1e6))
        dev.run("shell", "am force-stop " + PACKAGE)
        install = dev.run("install", "-r", "-t", apk, timeout=300)
        if "Success" not in install.stdout:
            raise RuntimeError("adb install failed: " + (install.stdout + install.stderr).strip()[-300:])
        dev.sh("pm clear " + PACKAGE)  # a known empty app; this also drops permissions, so grant after
        for permission in PERMISSIONS:
            dev.run("shell", "pm grant %s %s" % (PACKAGE, permission))
        dev.run("shell", "input keyevent KEYCODE_HOME")
        dev.run("logcat", "-c")

        recorder = Recorder(ctx, dev, out)
        t0 = recorder.start()
        ctx.log("recorder: started")
        env = dict(os.environ)
        env.update({"E2E_OUT": out, "E2E_T0": "%.3f" % t0, "E2E_ADB": adb, "ANDROID_SERIAL": dev.serial,
                    "E2E_SERIAL": dev.serial, "E2E_PACKAGE": PACKAGE, "E2E_FFMPEG_BIN": common.tool("ffmpeg"),"E2E_PLATFORM": "android", "E2E_SABOTAGE": "1" if sabotage else "0"})
        cmd = [sys.executable, "-I", "-B", driver] if driver.endswith(".py") else \
            [shutil.which("pwsh") or "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", driver]
        ctx.log("driver: %s (launching %.1fs after the recorder)" % (os.path.basename(driver), time.time() - t0))
        with open(driver_log, "w", encoding="utf-8", errors="replace") as log:
            try:
                code = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, env=env, timeout=DRIVER_SECONDS).returncode
            except subprocess.TimeoutExpired:
                code = 124
                log.write("\ndriver timed out after %ds\n" % DRIVER_SECONDS)
        time.sleep(1.5)  # the last step stays on camera
        outcome.seconds = recorder.stop()
        ctx.log("recorder: stopped after %.1fs; driver exit %s" % (outcome.seconds, code))
        outcome.recording = recorder.collect(os.path.join(out, "android-raw.mp4"), outcome.seconds)
        outcome.steps = steps_path if os.path.isfile(steps_path) else ""
        outcome.passed = code == 0
        if code != 0:
            failed = [s for s in common.read_steps(steps_path) if not s.get("ok", True)]
            outcome.note = "driver exit %s%s" % (code, "; failed: %s" % failed[0].get("step") if failed else "")
        with open(logcat_path, "w", encoding="utf-8", errors="replace") as f:
            f.write(dev.run("logcat", "-d", "-v", "threadtime", timeout=60).stdout)
    except Exception as err:  # the lab turns this into a failed platform with a note
        outcome.note = "%s: %s" % (type(err).__name__, err)
        ctx.log("android: " + outcome.note)
        ctx.log(traceback.format_exc())
        outcome.passed = False
        if recorder is not None and recorder.t0 and not outcome.recording:
            try:
                outcome.seconds = recorder.stop()
                outcome.recording = recorder.collect(os.path.join(out, "android-raw.mp4"), outcome.seconds)
            except Exception as err2:
                ctx.log("recorder: could not save: %s" % err2)
    finally:
        try:
            dev.run("shell", "am force-stop " + PACKAGE)
        finally:
            emulator.stop()
    ctx.log("android: %s in %.0fs wall" % ("passed" if outcome.passed else "failed", time.time() - started))
    return outcome
