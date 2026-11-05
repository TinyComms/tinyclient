"""One running app instance (a "device") the device-link proof drives, wherever it runs.

Every device is the real app: chatd (which holds the account, E2EE state and the sealed cache) plus,
where there is a screen to film, the tinyclient window on top of it. The proof asks chatd through
its own IPC (dlctl, a client over the app's `protocol` crate), which is what the window does.

  WinLocal   the coordinator. A scratch TINYCLIENT_DIR per device, so Ada A, Ada B and Bea run side by
             side next to the user's own tinyclient without touching it. Window placed then sent
             behind everything; filmed with lib/wingrab.ps1 (PrintWindow), never the desktop.
  LinuxSsh   tiny-linux (plain `ssh tiny-linux`): the same, on the VM's X11 :0, filmed by
             x11grab as the lab does. dlctl and the app are built there (prepare-linux.sh).
  WinVmSsh   winbuild (plain `ssh winbuild`): chatd over ssh (no screen needed for the proof);
             the window and the gdigrab recorder need the console session (lab's run-session.ps1
             pattern), see CHECKLIST.
Phones are in mobile/ (chatd runs inside the app there).

Each device writes nothing outside its own folder, and stops only processes it started.
"""

import base64
import json
import os
import shlex
import subprocess
import time

CREATE_NO_WINDOW = 0x08000000
TINY = "tiny"


def b64(s):
    return base64.b64encode(s.encode()).decode()


class DeviceGone(RuntimeError):
    pass


class Device:
    name = "?"
    platform = "?"
    user_id = ""
    log = print

    # -- IPC -------------------------------------------------------------------------------------
    def ipc(self, req, timeout=70):
        raise NotImplementedError

    def account(self):
        return {"network": TINY, "account": self.user_id}

    def key(self, conv_id):
        return {"network": TINY, "account": self.user_id, "id": conv_id}

    def e2ee(self, ask, timeout=70):
        return self.ipc({"op": "tiny_e2ee", "account": self.account(), "ask": ask}, timeout=timeout)

    def e2ee_status(self):
        code, ans = self.e2ee({"op": "status"})
        return ans.get("status") if code == 0 and isinstance(ans, dict) else None

    def messages(self, conv_id, limit=200):
        code, ans = self.ipc({"op": "messages", "conversation": self.key(conv_id), "limit": limit})
        if code != 0 or not isinstance(ans, dict):
            return None
        for k in ("messages", "items", "page"):
            if isinstance(ans.get(k), list):
                return ans[k]
        # unknown shape: the first list of dicts with an id
        for v in ans.values():
            if isinstance(v, list) and v and isinstance(v[0], dict) and "id" in v[0]:
                return v
        return []

    def wait_until(self, what, fn, seconds=60, every=0.5):
        """Retries fn() until it returns something truthy; the driver's bounded wait, not a poll of ours."""
        deadline = time.time() + seconds
        last = None
        while time.time() < deadline:
            try:
                last = fn()
            except DeviceGone:
                raise
            except Exception as e:  # a device mid-restart
                last = None
                self.log(f"{self.name}: {what}: {e}")
            if last:
                return last
            time.sleep(every)
        return last

    def ready(self, seconds=90):
        return self.wait_until("chatd up", lambda: self.ipc({"op": "status"}, timeout=10)[0] == 0, seconds)

    # -- screen ----------------------------------------------------------------------------------
    def shot(self, path):
        return False

    def start(self):
        raise NotImplementedError

    def stop(self):
        raise NotImplementedError

    def collect(self, out_dir):
        pass


class WinLocal(Device):
    platform = "windows"

    def __init__(self, name, session, app_dir, dlctl, state, label, gui=True, extra_env=None, log=print):
        self.name, self.session, self.app_dir, self.dlctl = name, session, app_dir, dlctl
        self.user_id = session["user_id"]
        self.dir = os.path.join(state, "devices", name)
        self.label, self.gui, self.extra_env, self.log = label, gui, extra_env or {}, log
        self.chatd = self.window = None
        self.win = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "scripts", "e2e", "win-window.ps1"))

    def env(self):
        e = dict(os.environ)
        e.update({"TINYCLIENT_DIR": self.dir, "TINYCLIENT_LOG_LEVEL": "debug"})
        e["TINYCLIENT_SESSION_TINY SERVER"] = json.dumps(self.session)
        e.update(self.extra_env)
        return e

    def start(self, fresh=True):
        if fresh and os.path.isdir(self.dir):
            import shutil
            shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(os.path.join(self.dir, "outbox"), exist_ok=True)
        for attempt in range(3):
            out = open(os.path.join(self.dir, "chatd.console.log"), "a")
            self.chatd = subprocess.Popen([os.path.join(self.app_dir, "chatd.exe")], env=self.env(), cwd=self.dir,
                                          stdout=out, stderr=subprocess.STDOUT, creationflags=CREATE_NO_WINDOW)
            time.sleep(2)
            # "Access is denied" at start: a chatd of an earlier run on this dir still held its pipe
            # for a moment after it was stopped. Try again shortly.
            if self.chatd.poll() is None:
                break
            self.log(f"{self.name}: chatd exited at start ({self.chatd.returncode}), retrying")
            time.sleep(5)
        if not self.ready():
            return False
        if self.gui:
            self.window = subprocess.Popen([os.path.join(self.app_dir, "tinyclient.exe")], env=self.env(), cwd=self.dir)
            placed = self.wait_until("window", lambda: self._win("rect") is not None, 60, 1)
            if placed:
                i = {"a_desk_a": 0, "a_desk_b": 1, "b_desk": 2}.get(self.name, 3)
                self._win("place", X=40 + 60 * i, Y=40 + 40 * i, W=1280, H=800)
                time.sleep(1)
                self._win("back")
        return True

    def _win(self, action, **kw):
        if not self.window:
            return None
        args = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", self.win, "-ProcessId", str(self.window.pid), "-Action", action]
        for k, v in kw.items():
            args += [f"-{k}", str(v)]
        r = subprocess.run(args, capture_output=True, text=True, creationflags=CREATE_NO_WINDOW)
        return r.stdout if r.returncode == 0 else None

    def shot(self, path):
        return self._win("shot", Path=path) is not None

    def ipc(self, req, timeout=70):
        if self.chatd and self.chatd.poll() is not None:
            # When its window exits, the app stops chatd (even one it did not start). A run that goes
            # on without the device proves nothing about it: stop here, loudly.
            raise DeviceGone(f"{self.label}: chatd exited ({self.chatd.returncode}); "
                             f"window {'gone' if self.window and self.window.poll() is not None else 'still up'}")
        try:
            r = subprocess.run([self.dlctl, self.dir, json.dumps(req)], capture_output=True, text=True, timeout=timeout,
                               creationflags=CREATE_NO_WINDOW)
        except subprocess.TimeoutExpired:
            return 5, None
        try:
            return r.returncode, json.loads(r.stdout) if r.stdout.strip() else r.stderr.strip()
        except ValueError:
            return r.returncode, r.stdout.strip()

    def outbox(self):
        return os.path.join(self.dir, "outbox")

    def read_file(self, path):
        with open(path, "rb") as file:
            return file.read()

    def pids(self):
        return [p.pid for p in (self.window, self.chatd) if p]

    def stop(self):
        # The window restarts chatd if its chatd goes away first, so the window goes first, and any
        # chatd the window started is found by its parent and stopped too (only those).
        spawned = []
        if self.window:
            r = subprocess.run(["powershell.exe", "-NoProfile", "-Command",
                                f"(Get-CimInstance Win32_Process -Filter \"ParentProcessId={self.window.pid}\").ProcessId"],
                               capture_output=True, text=True, creationflags=CREATE_NO_WINDOW)
            spawned = [int(x) for x in r.stdout.split() if x.strip().isdigit()]
        for p in (self.window, self.chatd):
            if p and p.poll() is None:
                p.kill()
        for pid in spawned:
            subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True, creationflags=CREATE_NO_WINDOW)
        # chatd's mediad child, if a call ever started (it does not in this proof), dies with chatd.

    def collect(self, out_dir):
        import shutil
        os.makedirs(out_dir, exist_ok=True)
        for f in ("logs", "chatd.console.log"):
            src = os.path.join(self.dir, f)
            if os.path.isdir(src):
                shutil.copytree(src, os.path.join(out_dir, f), dirs_exist_ok=True)
            elif os.path.isfile(src):
                shutil.copy2(src, out_dir)


class LinuxSsh(Device):
    """tiny-linux. The host side is mobile-free bash in linux-host.sh (copied there by prepare)."""
    platform = "linux"

    def __init__(self, name, session, host, work, label, log=print):
        self.name, self.session, self.host, self.work, self.label, self.log = name, session, host, work, label, log
        self.user_id = session["user_id"]
        self.dir = f"/tmp/tiny-dl-{name}"   # short: chatd's socket path has a 108-byte limit

    def sh(self, cmd, timeout=120):
        r = subprocess.run(["ssh", "-n", self.host, cmd], capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr

    def start(self, fresh=True):
        sess = b64(json.dumps(self.session))
        code, out, err = self.sh(f"bash {self.work}/linux-host.sh start {self.name} {sess} {'fresh' if fresh else 'keep'}", timeout=180)
        self.log(f"{self.name}: start: {out.strip()[-300:]} {err.strip()[-300:]}")
        return code == 0 and self.ready()

    def ipc(self, req, timeout=70):
        try:
            code, out, err = self.sh(f"{self.work}/bin/dlctl {self.dir}/data @b64:{b64(json.dumps(req))}", timeout=timeout + 15)
        except subprocess.TimeoutExpired:
            return 5, None
        try:
            return code, json.loads(out) if out.strip() else err.strip()
        except ValueError:
            return code, out.strip()

    def shot(self, path):
        code, _, _ = self.sh(f"bash {self.work}/linux-host.sh shot {self.name} /tmp/tiny-dl-shot.png")
        if code != 0:
            return False
        return subprocess.run(["scp", "-q", f"{self.host}:/tmp/tiny-dl-shot.png", path]).returncode == 0

    def record_start(self):
        return self.sh(f"bash {self.work}/linux-host.sh rec-start {self.name}")[0] == 0

    def record_stop(self, local_path):
        self.sh(f"bash {self.work}/linux-host.sh rec-stop {self.name}")
        return subprocess.run(["scp", "-q", f"{self.host}:/tmp/tiny-dl-rec-{self.name}/screen.mp4", local_path]).returncode == 0

    def outbox(self):
        return f"{self.dir}/data/outbox"

    def read_file(self, path):
        code, out, err = self.sh("base64 -w0 -- " + shlex.quote(path))
        if code != 0:
            raise RuntimeError("cannot read downloaded file: " + err[-300:])
        return base64.b64decode(out, validate=True)

    def stop(self):
        self.sh(f"bash {self.work}/linux-host.sh stop {self.name}")

    def collect(self, out_dir):
        os.makedirs(out_dir, exist_ok=True)
        self.sh(f"tar czf /tmp/tiny-dl-{self.name}-logs.tgz -C {self.dir} data/logs chatd.log tinyclient.log 2>/dev/null; true")
        subprocess.run(["scp", "-q", f"{self.host}:/tmp/tiny-dl-{self.name}-logs.tgz", os.path.join(out_dir, "logs.tgz")])


class WinVmSsh(Device):
    """winbuild: chatd started over ssh with the device's scratch dir (vm-host.ps1)."""
    platform = "windows-vm"

    def __init__(self, name, session, host, work, label, log=print):
        self.name, self.session, self.host, self.work, self.label, self.log = name, session, host, work, label, log
        self.user_id = session["user_id"]
        self.dir = f"{work}\\dl\\{name}"

    def ps(self, args, timeout=120):
        r = subprocess.run(["ssh", self.host, f"powershell -NoProfile -ExecutionPolicy Bypass -File {self.work}\\dl\\vm-host.ps1 {args}"],
                           capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr

    def start(self, fresh=True):
        code, out, err = self.ps(f"start {self.name} {b64(json.dumps(self.session))} {'fresh' if fresh else 'keep'}", timeout=180)
        self.log(f"{self.name}: start: {out.strip()[-300:]} {err.strip()[-300:]}")
        return code == 0 and self.ready()

    def ipc(self, req, timeout=70):
        try:
            r = subprocess.run(["ssh", self.host, f"{self.work}\\dl\\dlctl.exe {self.dir} @b64:{b64(json.dumps(req))}"],
                               capture_output=True, text=True, timeout=timeout + 15)
        except subprocess.TimeoutExpired:
            return 5, None
        try:
            return r.returncode, json.loads(r.stdout) if r.stdout.strip() else r.stderr.strip()
        except ValueError:
            return r.returncode, r.stdout.strip()

    def outbox(self):
        return f"{self.dir}\\outbox"

    def stop(self):
        self.ps(f"stop {self.name}")

    def collect(self, out_dir):
        os.makedirs(out_dir, exist_ok=True)
        subprocess.run(["scp", "-q", "-r", f"{self.host}:{self.dir.replace(chr(92), '/')}/logs", out_dir])
