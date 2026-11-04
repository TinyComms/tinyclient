"""Shared pieces of the lab: what a platform adapter is handed and returns, and the checks every
recording must pass before the lab will post a status for it. Standard library only.

e2e/README.md says what a lab run is. This file is that, in code.
"""
import dataclasses
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

MIN_SHORT_SIDE = 360
CAPTION_FPS = 10


def tool(name):
    """ffmpeg or ffprobe: $E2E_FFMPEG's folder, then PATH."""
    override = os.environ.get("E2E_FFMPEG")
    if override:
        candidate = os.path.join(os.path.dirname(override), name + (".exe" if os.name == "nt" else ""))
        if os.path.exists(candidate):
            return candidate
    found = shutil.which(name)
    if not found:
        raise SystemExit("lab: %s is not on PATH (set E2E_FFMPEG to ffmpeg's full path)" % name)
    return found


@dataclasses.dataclass
class Run:
    """One build of the app driven by one scenario, on one platform. What an adapter is given."""
    platform: str
    label: str            # "base" or "branch"
    repo: str             # "TinyComms/tinyclient"
    app_sha: str          # the commit to build and run
    scenario_sha: str     # the commit the scenario's files come from (the pull request's head)
    scenario: dict        # scenario.json
    scenario_dir: str     # local folder holding the scenario's files at scenario_sha
    source_tar: str       # local tar of the source at app_sha, made with core.autocrlf=false
    out_dir: str          # local folder the adapter puts everything it brings back in
    host: dict            # this platform's entry in hosts.json
    log: object = print   # log(line)


@dataclasses.dataclass
class Outcome:
    """What an adapter brings back. `recording` is the raw capture; the lab validates and captions it."""
    passed: bool
    recording: str = ""   # local path of the raw video, "" if none was made
    steps: str = ""       # local path of steps.jsonl (t = seconds since the recorder started)
    seconds: float = 0.0  # wall time from recorder start to recorder stop
    logs: list = dataclasses.field(default_factory=list)
    binary_sha256: str = ""
    note: str = ""
    # The recorder writes frames only when the screen changes (simctl, screenrecord), so the file ends
    # at the last change and is shorter than the run. The lab pads it; it does not call that a gap.
    vfr: bool = False


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd, check=True, **kw):
    return subprocess.run(cmd, check=check, capture_output=True, text=True, **kw)


def read_steps(path):
    if not path or not os.path.isfile(path):
        return []
    steps = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                steps.append(json.loads(line))
    return steps


def probe(path):
    out = run([tool("ffprobe"), "-v", "error", "-select_streams", "v:0",
               "-show_entries", "stream=width,height:format=duration", "-of", "json", path], check=False)
    if out.returncode != 0 or not out.stdout.strip():
        return None
    data = json.loads(out.stdout)
    if not data.get("streams"):
        return None
    stream = data["streams"][0]
    return {"width": int(stream["width"]), "height": int(stream["height"]),
            "seconds": float(data["format"].get("duration") or 0)}


def detect(path, total):
    """Seconds the picture was black and seconds it did not change, whole run and longest still."""
    out = run([tool("ffmpeg"), "-hide_banner", "-nostats", "-i", path, "-an",
               "-vf", "freezedetect=n=-55dB:d=1,blackdetect=d=1:pix_th=0.10", "-f", "null", "-"], check=False)
    text = out.stderr
    starts = [float(x) for x in re.findall(r"freeze_start: ([0-9.]+)", text)]
    freezes = [float(x) for x in re.findall(r"freeze_duration: ([0-9.]+)", text)]
    # A freeze that lasts to the end of the file logs its start and never its duration.
    if len(starts) > len(freezes):
        freezes.append(max(0.0, total - starts[-1]))
    blacks = [float(x) for x in re.findall(r"black_duration:([0-9.]+)", text)]
    return {"frozen": sum(freezes), "longest_still": max(freezes, default=0.0), "black": sum(blacks)}


def validate(path, seconds, steps, spec=None, expect_pass=True, vfr=False):
    """The reasons this recording proves nothing. Empty when it is good.

    A run that was meant to fail (the base build, in the red half of red-then-green) is judged only
    on whether it plays and shows its steps: a failing app is allowed to sit still or go dark, and
    that is the evidence."""
    spec = spec or {}
    if not path or not os.path.isfile(path):
        return ["there is no recording"]
    info = probe(path)
    if info is None:
        return ["the recording does not play"]
    problems = []
    if min(info["width"], info["height"]) < MIN_SHORT_SIDE:
        problems.append("the picture is %dx%d; the short side must be at least %d" % (info["width"], info["height"], MIN_SHORT_SIDE))
    span = max([s.get("t", 0) for s in steps], default=0)
    if not steps:
        problems.append("the driver wrote no steps, so nothing says what happened on camera")
    covered = max(info["seconds"], seconds or 0) if vfr else info["seconds"]
    if span > covered + 1.5:
        problems.append("the last step is at %.1fs but the recording covers %.1fs: steps happened off camera" % (span, covered))
    if not vfr and seconds and info["seconds"] + 2 < seconds:
        problems.append("the recording is %.1fs for a %.1fs run" % (info["seconds"], seconds))
    if not expect_pass:
        return problems
    shown = detect(path, info["seconds"])
    length = max(info["seconds"], 0.001)
    if shown["black"] / length >= 0.5:
        problems.append("the picture is black for %.0f%% of the run" % (100 * shown["black"] / length))
    # Most of a UI recording is a screen holding still. What matters is that it moved at all.
    if length > 3 and length - shown["frozen"] < 1.0:
        problems.append("the picture never changes: %.1fs of motion in %.0fs" % (length - shown["frozen"], length))
    limit = spec.get("max_still_seconds", 20)
    if shown["longest_still"] > limit:
        problems.append("the picture is frozen for %.0fs in one stretch (limit %ds)" % (shown["longest_still"], limit))
    return problems


def drawtext_escape(text):
    return (text.replace("\\", "\\\\").replace(":", "\\:").replace("'", "\u2019")
            .replace("%", " pct").replace(",", "\\,").replace("[", "(").replace("]", ")"))


def find_font():
    for candidate in (os.environ.get("E2E_FONT"), r"C:\Windows\Fonts\consola.ttf", r"C:\Windows\Fonts\arial.ttf",
                      "/System/Library/Fonts/Menlo.ttc", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"):
        if candidate and os.path.isfile(candidate):
            return candidate
    raise SystemExit("lab: no font for the caption (set E2E_FONT to a .ttf path)")


def caption(raw, out, header, steps, seconds):
    """Re-encode `raw` at a steady frame rate, padded to the run's length, with the pull request's
    SHA and the current step burned in, so a clip cannot be moved to another change."""
    font = find_font().replace("\\", "/").replace(":", "\\:")
    info = probe(raw)
    pad = max(0.0, (seconds or 0) - info["seconds"]) if info else 0.0
    parts = ["fps=%d" % CAPTION_FPS, "tpad=stop_mode=clone:stop_duration=%.2f" % pad,
             "drawbox=x=0:y=0:w=iw:h=44:color=black@0.6:t=fill",
             "drawtext=fontfile='%s':text='%s':fontcolor=white:fontsize=16:x=8:y=6" % (font, drawtext_escape(header))]
    ordered = sorted(steps, key=lambda s: s.get("t", 0))
    for i, step in enumerate(ordered):
        start = step.get("t", 0)
        end = ordered[i + 1].get("t", 0) if i + 1 < len(ordered) else start + 3600
        mark = "ok" if step.get("ok", True) else "FAILED"
        parts.append("drawtext=fontfile='%s':text='%s':fontcolor=%s:fontsize=16:x=8:y=24:enable='between(t,%.2f,%.2f)'" % (
            font, drawtext_escape("%s %s" % (mark, step.get("step", ""))), "white" if step.get("ok", True) else "red", start, end))
    script = out + ".filters.txt"
    with open(script, "w", encoding="utf-8") as f:
        f.write(",\n".join(parts))
    # ffmpeg 7 reads an option's value from a file with "-/opt"; older ones had "-filter_script:v".
    for option in ("-/filter:v", "-filter_script:v"):
        done = run([tool("ffmpeg"), "-hide_banner", "-loglevel", "error", "-y", "-i", raw,
                    option, script, "-an", "-c:v", "libx264", "-preset", "veryfast", "-crf", "28",
                    "-pix_fmt", "yuv420p", "-movflags", "+faststart", out], check=False)
        if done.returncode == 0 or "Unrecognized option" not in done.stderr:
            break
    os.remove(script)
    if done.returncode != 0:
        raise RuntimeError("captioning failed: " + done.stderr[-400:])
    return out


def gh(*args, token=None, check=True):
    env = dict(os.environ)
    env["GH_TOKEN"] = token or lab_token()
    return subprocess.run(["gh", *args], check=check, capture_output=True, text=True, env=env)


_token = None


def lab_token():
    """The one account the lab posts as (e2e/gate.json `lab_actors`): $E2E_GH_TOKEN, else itz4blitz's."""
    global _token
    if _token is None:
        _token = os.environ.get("E2E_GH_TOKEN") or subprocess.run(
            ["gh", "auth", "token", "--user", os.environ.get("E2E_GH_USER", "itz4blitz")],
            capture_output=True, text=True, check=True).stdout.strip()
    return _token


def post_status(repo, sha, context, state, description, url=None):
    args = ["api", "repos/%s/statuses/%s" % (repo, sha), "-f", "state=" + state, "-f", "context=" + context,
            "-f", "description=" + description[:140]]
    if url:
        args += ["-f", "target_url=" + url]
    gh(*args)


def log_to(path):
    handle = open(path, "a", encoding="utf-8")

    def log(line):
        text = "%s" % line
        print(text, file=sys.stderr, flush=True)
        handle.write(text + "\n")
        handle.flush()
    return log
