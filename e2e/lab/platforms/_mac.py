"""What the macOS and iOS adapters share: the Mac (`mbp`) is one machine, so they ship the same
scripts to it and talk to it the same way. Standard library only. Not an adapter: lab.py loads
only platforms/<platform>.py.

Everything that runs on the Mac is in platforms/mac/ and lands in ~/tiny-lab (on the external
drive, through a symlink: the internal disk has no room for cargo's target directory).
"""
import io
import os
import re
import shlex
import subprocess
import tarfile
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
MAC = os.path.join(HERE, "mac")
WORK = "~/tiny-lab"
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=30"]


class MacError(RuntimeError):
    pass


def _ssh(host, remote_command, data=None, timeout=600):
    return subprocess.run([*SSH, host["ssh"], remote_command], input=data, capture_output=True, timeout=timeout)


def _stream(argv, data, log, timeout):
    """Runs `argv`, feeds it `data`, and streams its output to `log` until it ends or `timeout`
    seconds pass. The deadline is the caller's, not the child's: it holds when the child prints
    nothing at all (a stalled build, a hung disk, a driver waiting on a screen that never comes)."""
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    lines = []

    def pump():
        for raw in proc.stdout:
            line = raw.decode("utf-8", "replace").rstrip()
            if "can't change option: zle" in line:  # the login shell's noise on a non-terminal
                continue
            lines.append(line)
            log("mac: " + line)

    reader = threading.Thread(target=pump, daemon=True)
    reader.start()
    try:
        proc.stdin.write(data)
        proc.stdin.close()
    except OSError:
        pass  # the child is already gone; its exit status says why
    try:
        status = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        reader.join(5)
        raise MacError("the Mac did not finish within %d s" % timeout)
    reader.join(30)
    return status, "\n".join(lines)


def remote(host, script, env=None, log=print, timeout=3600):
    """Runs a shell script on the Mac (login zsh, then sh) with `env` exported first, streaming its
    output to `log`. Returns (exit status, output text). Raises MacError at `timeout` seconds."""
    exports = "".join("export %s=%s\n" % (k, shlex.quote(str(v))) for k, v in (env or {}).items())
    command = 'f=$(mktemp /tmp/lab.XXXXXX); cat > "$f"; zsh -lc "sh $f"; rc=$?; rm -f "$f"; exit $rc'
    return _stream([*SSH, host["ssh"], command], (exports + script).encode("utf-8"), log, timeout)


def _tar_of(folder, rename=None):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tar:
        for base, _, files in os.walk(folder):
            for name in sorted(files):
                path = os.path.join(base, name)
                rel = os.path.relpath(path, folder).replace(os.sep, "/")
                if rename:
                    rel = rename(rel)
                info = tar.gettarinfo(path, arcname=rel)
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                info.mode = 0o755 if name.endswith(".sh") else 0o644
                with open(path, "rb") as f:
                    data = f.read()
                if name.endswith((".sh", ".json", ".swift", ".yml", ".plist")):
                    data = data.replace(b"\r\n", b"\n")  # a Windows checkout must not break `sh`
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def ship_scripts(host):
    """platforms/mac/ into ~/tiny-lab (helper/ becomes helper-src/). Cheap and idempotent."""
    data = _tar_of(MAC, lambda rel: "helper-src/" + rel[len("helper/"):] if rel.startswith("helper/") else rel)
    done = _ssh(host, "mkdir -p ~/Development/tiny-lab && { [ -e ~/tiny-lab ] || ln -s ~/Development/tiny-lab ~/tiny-lab; } "
                      "&& tar -xf - -C ~/tiny-lab", data=data, timeout=120)
    if done.returncode != 0:
        raise MacError("could not ship the lab scripts to the Mac: " + done.stderr.decode("utf-8", "replace")[-300:])


def put(host, local, remote_path):
    done = subprocess.run(["scp", "-q", "-o", "BatchMode=yes", local, "%s:%s" % (host["ssh"], remote_path)],
                          capture_output=True, text=True, timeout=900)
    if done.returncode != 0:
        raise MacError("scp to the Mac failed: " + done.stderr[-300:])


def put_dir(host, local_dir, remote_dir):
    done = _ssh(host, "rm -rf %s && mkdir -p %s && tar -xf - -C %s" % (remote_dir, remote_dir, remote_dir), data=_tar_of(local_dir), timeout=120)
    if done.returncode != 0:
        raise MacError("could not copy %s to the Mac: %s" % (local_dir, done.stderr.decode("utf-8", "replace")[-300:]))


def fetch_dir(host, remote_dir, local_dir):
    os.makedirs(local_dir, exist_ok=True)
    done = _ssh(host, "tar -cf - -C %s ." % remote_dir, timeout=900)
    if done.returncode != 0:
        raise MacError("could not fetch %s: %s" % (remote_dir, done.stderr.decode("utf-8", "replace")[-300:]))
    with tarfile.open(fileobj=io.BytesIO(done.stdout)) as tar:
        tar.extractall(local_dir, filter="data")


def value(text, key):
    """The last `KEY=value` line a Mac script printed."""
    found = re.findall(r"^(?:mac: )?%s=(.*)$" % re.escape(key), text, re.M)
    return found[-1].strip() if found else ""


def tidy(host, run_id, remote_tar):
    """This run's folder (it holds a copy of the app) and source tar off the Mac; old ones too."""
    _ssh(host, "rm -rf /tmp/tiny-lab-runs/%s /tmp/tiny-lab-runs/%s.build.log %s; "
               "find /tmp/tiny-lab-runs -maxdepth 1 -mindepth 1 -mmin +2880 -exec rm -rf {} + 2>/dev/null; true" % (run_id, run_id, remote_tar), timeout=120)
