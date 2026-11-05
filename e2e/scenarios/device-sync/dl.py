"""Device linking + history transfer: the end-to-end proof, on real app instances.

  python dl.py run [options]        the scenario (happy path, then the negative cases)
  python dl.py link-ops             prints the link step mapping in use (link-ops.json)

Ada (desktop A, the coordinator) has an encrypted DM with Bea and an encrypted channel, with history sent
BEFORE any link: text, an edit, a reply and a picture, each carrying a canary string. Ada then links
desktop B (a second profile on the coordinator), and desktop C (tiny-linux and/or winbuild). Each new
device must read the pre-link history, receive new messages; Bea must see one Ada; a revoked device
must stop reading. Then the negative cases, then the server-side canary check.

Everything a step proves is asserted from the app's own state (chatd, over its IPC: what the
window draws) and, for the link protocol, from the wire (lib/linkproxy.py). Screens are filmed.

Output (lab format, per platform): <out>/<platform>/steps.jsonl, shots/, recording, logs; plus
<out>/summary.json (machine-readable pass/fail) and <out>/summary.md.

Run `python dl.py run --help`. README.md has the exact commands.
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "lib"))
from devices import WinLocal, LinuxSsh, WinVmSsh  # noqa: E402
from attachments import verify_picture

CLIENT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
SPACE = os.path.dirname(CLIENT)
LAB = os.path.join(CLIENT, "e2e", "lab")
STATE = os.path.join(os.environ.get("LOCALAPPDATA", "/tmp"), "tiny-e2e", "device-sync")
DLCTL = os.path.join(os.environ.get("LOCALAPPDATA", "/tmp"), "tiny-e2e", "dlctl-target", "release", "dlctl.exe")
INSTALLED = os.path.join(os.environ.get("LOCALAPPDATA", ""), "Programs", "tinyclient")


# ------------------------------------------------------------------------------------------------
# steps, in the lab's steps.jsonl format, one file per platform; plus the assertion ledger
# ------------------------------------------------------------------------------------------------
class Ledger:
    def __init__(self, out, t0):
        self.out, self.t0 = out, t0
        self.assertions = []
        self.files = {}
        self.failed_steps = []

    def steps_file(self, platform):
        d = os.path.join(self.out, platform)
        os.makedirs(os.path.join(d, "shots"), exist_ok=True)
        return os.path.join(d, "steps.jsonl")

    def step(self, platform, words, ok, shot="", **detail):
        line = {"t": round(time.time() - self.t0, 1), "step": words, "ok": bool(ok)}
        if shot:
            line["shot"] = shot
        with open(self.steps_file(platform), "a", encoding="utf-8") as f:
            f.write(json.dumps(line) + "\n")
        if not ok:
            self.failed_steps.append({"platform": platform, "step": words})
        mark = "ok    " if ok else "FAILED"
        print(f"[{line['t']:7.1f}s] {platform:10s} {mark} {words}" + (f"  {json.dumps(detail)[:240]}" if detail and not ok else ""), flush=True)
        return ok

    def check(self, aid, title, ok, platform="windows", expected_on_base=None, waiting_on=None, **detail):
        """One assertion. `expected_on_base`: what main (no device link) gives, so a base run that
        fails exactly these is the red half of red-then-green. `waiting_on`: the piece of the
        implementation without which this cannot pass (for the checklist)."""
        rec = {"id": aid, "title": title, "platform": platform, "ok": ok, "detail": detail}
        if expected_on_base is not None:
            rec["expected_on_base"] = expected_on_base
        if waiting_on:
            rec["waiting_on"] = waiting_on
        self.assertions.append(rec)
        if ok is None:
            print(f"           {platform:10s} SKIP   {aid} {title}  ({detail.get('why', '')})", flush=True)
            return None
        self.step(platform, f"{aid}: {title}", ok, **detail)
        return ok


# ------------------------------------------------------------------------------------------------
# small helpers
# ------------------------------------------------------------------------------------------------
def rest(base, token, method, path, body=None):
    req = urllib.request.Request(base + path, method=method, data=json.dumps(body).encode() if body is not None else None,
                                 headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, None


def find(obj, pattern):
    """First string anywhere in obj that matches `pattern`."""
    rx = re.compile(pattern)
    stack = [obj]
    while stack:
        v = stack.pop()
        if isinstance(v, str):
            m = rx.search(v)
            if m:
                return m.group(0)
        elif isinstance(v, dict):
            stack.extend(v.values())
        elif isinstance(v, list):
            stack.extend(v)
    return None


def fill(template, values):
    """A link-ops request template with {placeholders} filled, recursively."""
    if isinstance(template, str):
        for k, v in values.items():
            template = template.replace("{" + k + "}", str(v))
        return template
    if isinstance(template, dict):
        return {k: fill(v, values) for k, v in template.items()}
    if isinstance(template, list):
        return [fill(v, values) for v in template]
    return template


def make_picture(path, canary):
    """A real PNG, 640x400, with the canary drawn in it and in a tEXt chunk: if any byte of the
    plaintext picture reaches the server, the canary check finds it."""
    from PIL import Image, ImageDraw, PngImagePlugin
    img = Image.new("RGB", (640, 400), (32, 96, 160))
    d = ImageDraw.Draw(img)
    for i in range(0, 640, 40):
        d.line([(i, 0), (640 - i, 400)], fill=(255, 200, 0), width=3)
    d.rectangle([20, 160, 620, 240], fill=(255, 255, 255))
    d.text((32, 190), canary, fill=(0, 0, 0))
    meta = PngImagePlugin.PngInfo()
    meta.add_text("Comment", canary)
    img.save(path, pnginfo=meta)
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest(), os.path.getsize(path)


def ready_state(dev):
    s = dev.e2ee_status() or {}
    return (s.get("device") or {}).get("state"), s


def readable(msg):
    return msg.get("seal") != "undecryptable" and "Can't be decrypted" not in (msg.get("text") or "")


# ------------------------------------------------------------------------------------------------
# linking: today's approve-by-code flow ("legacy") or the device-link flow ("v1", link-ops.json)
# ------------------------------------------------------------------------------------------------
class Linker:
    def __init__(self, ops, ledger, run_dir, server):
        self.ops, self.L, self.run_dir, self.server = ops, ledger, run_dir, server
        self.mode = ops["mode"]
        self.secrets = []   # link codes seen: the canary check looks for them on the server

    def wire(self):
        path = os.path.join(STATE, "wire", "wire.jsonl")
        if not os.path.exists(path):
            return []
        return [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]

    def proxy(self, **mode):
        with open(self.server["proxy_control"], "w") as f:
            json.dump(mode, f)

    def link(self, old, new, platform, expect="success", history="all", reject=False, shots=True):
        """Links `new` from `old`. Returns a dict of what was seen. `expect` is what the case
        wants: success, abort (mismatch/rejected/tampered) or refused (expired/used)."""
        seen = {"mode": self.mode, "new": new.name, "old": old.name}
        wire_from = len(self.wire())
        if self.mode == "legacy":
            st = new.wait_until("awaiting approval", lambda: (lambda s: s if s[0] == "awaiting_approval" else None)(ready_state(new)), 60)
            if not st:
                seen["error"] = f"new device never asked for approval: {ready_state(new)[0]}"
                return seen
            code = find(st[1], r"tiny-device-[A-Za-z0-9_\-+/=]+")
            seen["code"] = code
            self.secrets.append(code)
            self.L.step(platform, f"{new.label} shows its device code (legacy flow)", bool(code))
            if shots:
                self.shot(new, platform, f"{new.name}-code")
            rc, ans = old.e2ee({"op": "approve", "code": code})
            seen["approve"] = ans
            self.L.step(platform, f"{old.label} approves {new.label} by its code", rc == 0, answer=ans)
        else:
            v1 = self.ops["v1"]
            ask = lambda dev, key, **kw: dev.ipc(fill(v1[key], dict(account=dev.user_id, **kw)))
            def state(dev):
                rc, ans = ask(dev, "state")
                return (ans or {}).get("link") or (ans or {}).get("status") or ans if rc == 0 and isinstance(ans, dict) else {}
            rc, ans = ask(new, "new_start")
            seen["new_start"] = {"rc": rc, "answer": ans}
            code = new.wait_until("link code", lambda: find(state(new), v1["code_regex"]) or find(new.e2ee_status(), v1["code_regex"]), 30)
            seen["code"] = code
            self.secrets.append(code)
            self.L.step(platform, f"{new.label} shows a link code", bool(code), answer=ans if not code else None)
            if not code:
                return self.finish(seen, wire_from)
            if shots:
                self.shot(new, platform, f"{new.name}-link-code")
            if expect == "refused-expired":
                wait = int(self.server.get("link_ttl_seconds") or 300) + 5
                self.L.step(platform, f"wait {wait}s so the link expires", True)
                time.sleep(wait)
            rc, ans = ask(old, "old_join", code=code)
            seen["old_join"] = {"rc": rc, "answer": ans}
            self.L.step(platform, f"{old.label} takes the code (pasted, as from a QR)", rc == 0 or expect.startswith("refused"), answer=ans if rc else None)
            # The new device shows six digits; the old one asks the person to type them.
            check_new = new.wait_until("check (new)", lambda: find(state(new), v1["check_regex"]), 45)
            old_stage = old.wait_until("old at confirm", lambda: (lambda st: st if st.get("stage") in ("confirm", "failed") else None)(state(old)), 45) or state(old)
            seen.update(check_new=check_new, old_state=old_stage, new_state=state(new))
            if shots:
                self.shot(old, platform, f"{old.name}-confirm")
                self.shot(new, platform, f"{new.name}-check")
            if expect.startswith("refused"):
                seen["refused"] = rc != 0 or old_stage.get("stage") == "failed" or not check_new
                return self.finish(seen, wire_from)
            self.L.step(platform, f"{new.label} shows a 6-digit check ({check_new}); {old.label} asks for it (stage {old_stage.get('stage')})",
                        bool(check_new) and old_stage.get("stage") == "confirm" or expect.startswith("abort"), old=old_stage)
            typed = check_new or "000000"
            if reject:
                typed = typed[:-1] + str((int(typed[-1]) + 1) % 10)   # the person types digits that do not match
            rc, ans = ask(old, "old_confirm", check=typed, history=history)
            seen["confirm"] = {"rc": rc, "answer": ans, "typed": typed}
            words = "types the WRONG digits" if reject else f"types the digits and approves ({history} history)"
            self.L.step(platform, f"{old.label}: the person {words}", (rc == 0) != (reject or expect.startswith("abort")) or expect == "abort",
                        answer=ans if rc else None)
            final = set(v1.get("stages_final", ["done", "failed"]))
            seen["new_final"] = new.wait_until("new final", lambda: (lambda st: st if st.get("stage") in final else None)(state(new)), 120) or state(new)
            seen["old_final"] = old.wait_until("old final", lambda: (lambda st: st if st.get("stage") in final else None)(state(old)), 30) or state(old)
            self.L.step(platform, f"link ends: new {seen['new_final'].get('stage')} ({seen['new_final'].get('messages', 0)} messages taken in), old {seen['old_final'].get('stage')}",
                        (seen["new_final"].get("stage") == "done") == (expect == "success"), new=seen["new_final"], old=seen["old_final"])
        return self.finish(seen, wire_from)

    def finish(self, seen, wire_from):
        seen["completed_at"] = time.time()
        time.sleep(2)
        seen["wire"] = [{k: r.get(k) for k in ("method", "route", "link", "status", "from", "injected", "resp_bytes")}
                        for r in self.wire()[wire_from:] if r.get("route")]
        return seen

    def shot(self, dev, platform, name):
        rel = f"shots/{name}.png"
        path = os.path.join(self.run_dir, platform, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ok = dev.shot(path)
        return rel if ok else ""


# ------------------------------------------------------------------------------------------------
# the scenario
# ------------------------------------------------------------------------------------------------
class Scenario:
    def __init__(self, a):
        self.a = a
        self.run_id = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
        self.out = os.path.abspath(a.out or os.path.join(SPACE, "_proof", "device-link", self.run_id))
        if os.path.isdir(self.out) and os.listdir(self.out):
            raise ValueError("evidence directory is not empty: " + self.out)
        os.makedirs(self.out, exist_ok=True)
        self.t0 = time.time()
        self.L = Ledger(self.out, self.t0)
        self.canary = f"DLCANARY-{self.run_id}"
        self.devices = {}
        self.history = []     # (conv, sent_by, msg_id, text, kind)
        self.pics = {}
        self.grabber = None
        self.send_failures = []
        self.warmups = []
        self.linked = []

    # -- setup ------------------------------------------------------------------------------------
    def server_up(self):
        if self.a.reset:
            args = ["pwsh", "-NoProfile", "-File", os.path.join(HERE, "server", "dl-server.ps1"), "up"]
            if self.a.server_dir:
                args += ["-TinyServerDir", self.a.server_dir]
            if self.a.link_ttl:
                args += ["-LinkTtlSeconds", str(self.a.link_ttl)]
            if self.a.server_ref is not None:
                args += ["-TinyServerRef", self.a.server_ref]
            log = open(os.path.join(self.out, "server-up.log"), "w")
            r = subprocess.run(args, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
            if r.returncode != 0:
                raise SystemExit(f"server up failed, see {log.name}")
        self.seed = json.load(open(os.path.join(STATE, "seed.json"), encoding="utf-8-sig"))
        self.server = json.load(open(os.path.join(STATE, "server.json"), encoding="utf-8-sig"))
        self.base = self.server["public_url"]
        with open(self.server["proxy_control"], "w") as f:
            f.write("{}")
        ops = json.load(open(os.path.join(HERE, "link-ops.json"), encoding="utf-8"))
        if self.a.mode != "auto":
            ops["mode"] = self.a.mode
        elif ops.get("mode", "auto") == "auto":
            ops["mode"] = "v1" if self.server.get("e2ee_device_link") else "legacy"
        self.linker = Linker(ops, self.L, self.out, self.server)
        self.mode = ops["mode"]
        print(f"server {self.server['tinyserver_branch']}@{self.server['tinyserver_sha'][:9]} at {self.base}; "
              f"device link advertised: {self.server.get('e2ee_device_link')}; link mode: {self.mode}")

    def dev(self, name, label, where="local", gui=True):
        s = self.seed["sessions"][name]
        if where == "local":
            # Loopback, not the LAN address: chatd uploads over plain http only to 127.0.0.1/localhost
            # (crates/chatd/src/upload.rs uploads_to), and the pictures in the history must upload.
            s = dict(s, base_url=f"http://localhost:{self.server['proxy_port']}")
            d = WinLocal(name, s, self.a.app_dir, DLCTL, STATE, label, gui=gui)
        elif where == "linux":
            d = LinuxSsh(name, s, "tiny-linux", "~/tiny-dl", label)
        elif where == "winvm":
            d = WinVmSsh(name, s, "winbuild", "C:\\lab", label)
        self.devices[name] = d
        return d

    def grab_windows(self):
        """(Re)writes the grabber's window list: every local device with a window."""
        local = [d for d in self.devices.values() if isinstance(d, WinLocal) and d.window and d.window.poll() is None]
        with open(os.path.join(self.out, "windows", "grab-pids.txt"), "w") as f:
            f.write("".join(f"{d.window.pid} {d.label}\n" for d in local))

    def start_grabber(self):
        if self.a.no_record or self.a.no_gui:
            return
        frames = os.path.join(self.out, "windows", "frames")
        os.makedirs(frames, exist_ok=True)
        self.grab_windows()
        self.grabber = subprocess.Popen(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                                         os.path.join(HERE, "lib", "wingrab.ps1"), "-OutDir", frames,
                                         "-PidFile", os.path.join(self.out, "windows", "grab-pids.txt"), "-Fps", "2"],
                                        creationflags=0x08000000)

    def open_dm_on_screen(self, d):
        """Clicks the first inbox row (the DM, newest) in a device's window, so the conversation, not
        an empty pane, is what the camera sees. Posted window messages: the cursor and focus stay with
        whoever is at the desk. Position measured from shots (top-left anchored layout)."""
        if isinstance(d, WinLocal) and d.window:
            d._win("post", X=200, Y=144)

    def stop_grabber(self):
        if not self.grabber:
            return
        frames = os.path.join(self.out, "windows", "frames")
        open(os.path.join(frames, "stop"), "w").close()
        try:
            self.grabber.wait(30)
        except subprocess.TimeoutExpired:
            self.grabber.kill()
        subprocess.run([sys.executable, os.path.join(HERE, "lib", "frames2mp4.py"), frames,
                        os.path.join(self.out, "windows", "steps.jsonl"), os.path.join(self.out, "windows", "recording.mp4"),
                        str(self.t0), f"device-link {self.run_id} windows (the coordinator: Ada A, Ada B, Bea)"])

    def shot_all(self, platform, name):
        for d in self.devices.values():
            if d.platform == platform or (platform == "windows" and isinstance(d, WinLocal)):
                self.linker.shot(d, platform, f"{name}-{d.name}")

    # -- history ------------------------------------------------------------------------------------
    def send(self, dev, conv, text, reply_to=None, files=None, wait=True):
        req = {"op": "send", "conversation": dev.key(conv), "client_id": str(uuid.uuid4()), "text": text}
        if reply_to:
            req["reply_to"] = reply_to
        if files:
            req["with"] = {"files": [{"path": p} for p in files]}
        rc, ans = dev.ipc(req)
        if rc != 0:
            self.send_failures.append({"device": dev.name, "text": text, "refused": ans})
            return None
        cid = req["client_id"]
        if not wait:
            return cid

        def sent():
            ms = dev.messages(conv) or []
            for m in ms:
                if m.get("client_id") == cid:
                    st = m.get("state") if isinstance(m.get("state"), dict) else {}
                    if st.get("state") == "failed":
                        return ("failed", st.get("reason"))
                    if m.get("id") and m.get("id") != cid:
                        return m["id"]
            return None
        got = dev.wait_until("sent", sent, 60)
        if isinstance(got, tuple):
            self.send_failures.append({"device": dev.name, "text": text, "reason": got[1]})
            # what a person does with "Try again in a moment": drop it and send again
            dev.ipc({"op": "discard", "conversation": dev.key(conv), "client_id": cid})
            return None
        return got

    def send_retrying(self, dev, conv, text, seconds=120, **kw):
        deadline = time.time() + seconds
        while time.time() < deadline:
            mid = self.send(dev, conv, text, **kw)
            if mid:
                return mid
            time.sleep(5)
        return None

    def warm(self, conv, a, b, what):
        """Both people read each other in the conversation before the history is sent, so the
        history that follows is readable by both before any link (the control). Today's app needs
        this: the first message of a new encrypted conversation does not open for the other person,
        and their reply fails with "This device isn't in this conversation's encryption yet. Try
        again in a moment." until the group catches up. Retried as a person would; what happened
        is kept in warmup.json (a finding about the app, apart from device linking)."""
        log = {"conv": what, "tries": []}
        for d in (a, b):
            d.ipc({"op": "messages", "conversation": d.key(conv), "limit": 20})
        ok_b = None
        for i in range(8):
            t = f"{self.canary} warm-up {what} from Ada #{i}"
            mid = self.send_retrying(a, conv, t, 60)
            ok_b = b.wait_until("warm b", lambda: any(m.get("id") == mid and readable(m) for m in (b.messages(conv) or [])), 15)
            log["tries"].append({"from": "Ada", "n": i, "sent": bool(mid), "read_by_bea": bool(ok_b)})
            if ok_b:
                break
        t2 = f"{self.canary} warm-up {what} from Bea"
        mid2 = self.send_retrying(b, conv, t2, 120)
        ok_a = a.wait_until("warm a", lambda: any(m.get("id") == mid2 and readable(m) for m in (a.messages(conv) or [])), 60)
        log["tries"].append({"from": "Bea", "sent": bool(mid2), "read_by_ada": bool(ok_a)})
        log["send_failures"] = list(self.send_failures)
        self.warmups.append(log)
        with open(os.path.join(self.out, "warmup.json"), "w") as f:
            json.dump(self.warmups, f, indent=1)
        return bool(ok_a and ok_b)

    def seed_history(self, A, Bea):
        ada_id, bea_id = self.seed["people"]["ada"]["user_id"], self.seed["people"]["bea"]["user_id"]
        rc, ans = A.ipc({"op": "start_direct", "person": A.key(bea_id)})
        self.dm = ans["conversation"]["key"]["id"]
        enc = ans["conversation"].get("encryption")
        self.L.step("windows", f"Ada opens a DM with Bea ({enc})", enc == "end_to_end", encryption=enc)
        Bea.ipc({"op": "start_direct", "person": Bea.key(ada_id)})   # what Bea does: opens it too
        st, ch = rest(self.base, self.seed["sessions"]["a_desk_a"]["token"], "POST", "/v1/channels", {"name": f"dl-proof-{self.run_id[-6:]}", "e2ee": True})
        self.ch = (ch or {}).get("id")
        self.L.step("windows", "Ada makes an encrypted channel", st == 201 and (ch or {}).get("e2ee") is True, status=st)
        self.L.check("H0", "the DM and the channel are encrypted and both people can read each other there (warm-up)",
                     self.warm(self.dm, A, Bea, "dm") and bool(self.ch) and self.warm(self.ch, A, Bea, "channel"), expected_on_base=True)

        n = self.a.history
        for conv, tag in ((self.dm, "dm"), (self.ch, "ch")):
            ids = []
            for i in range(n):
                who = A if i % 2 == 0 else Bea
                text = f"{self.canary}-{tag}-text-{i:02d} from {'Ada' if who is A else 'Bea'}"
                mid = self.send_retrying(who, conv, text, 60)
                ids.append(mid)
                self.history.append({"conv": conv, "tag": tag, "id": mid, "text": text, "kind": "text", "by": who.name})
            # an edit of Ada's first
            orig = next(h for h in self.history if h["conv"] == conv and h["by"] == A.name)
            edited = orig["text"].replace("text-", "EDITED-text-")
            rc, ans = A.ipc({"op": "edit", "conversation": A.key(conv), "message_id": orig["id"], "text": edited})
            self.L.step("windows", f"Ada edits a message in the {tag}", rc == 0, answer=ans if rc else None)
            orig.update(text=edited, kind="edited", original=orig["text"])
            # a reply by Bea to it
            rtext = f"{self.canary}-{tag}-reply from Bea"
            rid = self.send_retrying(Bea, conv, rtext, 60, reply_to=orig["id"])
            self.history.append({"conv": conv, "tag": tag, "id": rid, "text": rtext, "kind": "reply", "reply_to": orig["id"], "by": Bea.name})
            # a picture by Ada
            # %TEMP%: the app refuses files under AppData (where the scratch TINYCLIENT_DIRs are) except there.
            pics_dir = os.path.join(os.environ.get("TEMP", A.outbox()), f"tiny-dl-{self.run_id}")
            os.makedirs(pics_dir, exist_ok=True)
            pic = os.path.join(pics_dir, f"dl-{tag}.png")
            sha, size = make_picture(pic, f"{self.canary}-{tag}-PICTURE")
            self.pics[tag] = {"sha256": sha, "bytes": size, "name": os.path.basename(pic)}
            ptext = f"{self.canary}-{tag}-picture from Ada"
            pid = self.send_retrying(A, conv, ptext, 90, files=[pic])
            self.history.append({"conv": conv, "tag": tag, "id": pid, "text": ptext, "kind": "picture", "file": os.path.basename(pic), "by": A.name})
        self.L.step("windows", f"history sent before any link: {len(self.history)} messages in DM + channel", all(h["id"] for h in self.history),
                    missing=[h["text"] for h in self.history if not h["id"]])
        # Control: what Ada and Bea read now. Each new device must read the same.
        for d in (A, Bea):
            res = self.read_history(d)
            self.L.check("H1", f"{d.label} reads all {len(self.history)} pre-link messages (control)", res["ok"], expected_on_base=True, **res["why"])
        with open(os.path.join(self.out, "history.json"), "w") as f:
            json.dump({"dm": self.dm, "channel": self.ch, "messages": self.history, "pictures": self.pics,
                       "send_failures": self.send_failures}, f, indent=1)

    def read_history(self, d, seconds=5, paginate=True):
        """Every pre-link message, as `d` shows it now: text (the edited one), reply link, picture."""
        def look():
            # What a person sees opening each conversation and scrolling up to its start: the
            # newest page, then the page before the oldest shown, until nothing older comes. chatd
            # answers a page past its cache at once and fetches the rest from the server, which
            # then opens from the (imported) sealed cache, so an empty page is asked again shortly.
            got = {}
            for conv in (self.dm, self.ch):
                page = d.messages(conv) or []          # oldest first
                for m in page:
                    got[m.get("id")] = m
                if not paginate:
                    continue
                first = page[0].get("id") if page else None
                empty = 0
                while first and empty < 3:
                    rc, ans = d.ipc({"op": "messages", "conversation": d.key(conv), "page": {"at": "before", "id": first}, "limit": 100})
                    page = (ans or {}).get("messages", []) if rc == 0 and isinstance(ans, dict) else []
                    new = [m for m in page if m.get("id") not in got]
                    for m in page:
                        got[m.get("id")] = m
                    if new:
                        first, empty = page[0].get("id"), 0
                    else:
                        empty += 1
                        time.sleep(2)
            return got
        got = d.wait_until("history", lambda: (lambda g: g if all(h["id"] in g and readable(g[h["id"]]) for h in self.history) else None)(look()), seconds) or look()
        missing, unreadable, wrong_text, no_reply, no_pic = [], [], [], [], []
        for h in self.history:
            m = got.get(h["id"])
            if not m:
                missing.append(h["text"])
                continue
            if not readable(m):
                unreadable.append(h["text"])
                continue
            if (m.get("text") or "") != h["text"]:
                wrong_text.append({"want": h["text"], "got": m.get("text")})
            if h["kind"] == "reply" and h["reply_to"] not in json.dumps(m.get("reply_to") or m.get("reply") or ""):
                no_reply.append(h["text"])
            if h["kind"] == "picture" and h["file"] not in json.dumps(m.get("attachments") or m.get("files") or ""):
                no_pic.append(h["text"])
        why = {"missing": len(missing), "unreadable": len(unreadable), "wrong_text": wrong_text[:3], "reply_lost": no_reply, "picture_lost": no_pic,
               "examples": (missing + unreadable)[:3]}
        ok = not (missing or unreadable or wrong_text or no_reply or no_pic)
        return {"ok": ok, "why": why, "seen": got}

    # -- one new device ------------------------------------------------------------------------------
    def link_and_prove(self, A, Bea, N, platform, prefix):
        before = len(((A.e2ee_status() or {}).get("devices") or []))
        bea_view = Bea.e2ee({"op": "person", "user_id": A.user_id})[1]
        if not N.start():
            self.L.step(platform, f"{N.label} starts signed in as Ada", False)
            return
        self.grab_windows()
        self.L.step(platform, f"{N.label} starts signed in as Ada (a new device)", True, shot=self.linker.shot(N, platform, f"{prefix}-start"))
        seen = self.linker.link(A, N, platform)
        linked_at = seen.get("completed_at", time.time())
        json.dump(seen, open(os.path.join(self.out, platform, f"{prefix}-link.json"), "w"), indent=1)
        st = N.wait_until("ready", lambda: ready_state(N)[0] == "ready", 90)
        self.L.check(f"{prefix}.H2", f"{N.label} is linked: cross-signed and ready", bool(st), platform, expected_on_base=True, state=ready_state(N)[0])
        res = self.read_history(N, seconds=0, paginate=False)
        self.L.check(f"{prefix}.H3a", f"right after the link, {N.label} shows the pre-link history (all {len(self.history)} messages)", res["ok"], platform,
                     expected_on_base=False, waiting_on="client: imported history visible before any newer message arrives", **res["why"])
        # Send before reading/import assertions: scrolling history can itself take longer than
        # the server's membership batch window and would conceal a broken immediate join.
        sent_at = time.time()
        immediate = self.send_retrying(Bea, self.dm, f"{self.canary}-{prefix}-right-after-link from Bea", 60)
        got = N.wait_until("new msg", lambda: any(m.get("id") == immediate and readable(m) for m in (N.messages(self.dm) or [])), 60)
        self.L.check(f"{prefix}.H7a", f"{N.label} reads a message Bea sends right after the link", bool(got), platform,
                     expected_on_base=True, sent_after_link_s=round(sent_at - linked_at, 3), message_id=immediate)
        after = (A.e2ee_status() or {}).get("devices") or []
        self.linked.append(N)
        self.L.check(f"{prefix}.H2b", f"Ada A lists the new device ({before} -> {len(after)})", len(after) == before + 1, platform, expected_on_base=True)
        self.open_dm_on_screen(N)
        self.linker.shot(N, platform, f"{prefix}-history")
        if self.mode == "v1":
            w = seen.get("wire", [])
            routes = [(r["method"], r["route"]) for r in w]
            ok = (("POST", "links") in routes and ("PUT", "archive") in routes and ("GET", "archive") in routes
                  and routes.count(("DELETE", "link")) >= 1)
            self.L.check(f"{prefix}.H12", "the wire shows the link protocol: create, messages both ways, archive up once and down, delete", ok, platform,
                         expected_on_base=False, routes=routes)
        else:
            self.L.check(f"{prefix}.H12", "link protocol on the wire", None, platform, why="legacy mode (server does not advertise e2ee_device_link)")
        # A second message after opening the imported history. Observe commits for diagnostics,
        # without waiting for one before sending the message.
        committed = any(
            r.get("method") == "POST" and r.get("path", "").endswith("/commit") and r.get("status") == 204 and r.get("t", 0) > linked_at
            for r in self.linker.wire())
        t = f"{self.canary}-{prefix}-after-link from Bea"
        mid = self.send_retrying(Bea, self.dm, t, 60)
        got = N.wait_until("new msg", lambda: any(m.get("id") == mid and readable(m) for m in (N.messages(self.dm) or [])), 60)
        self.L.check(f"{prefix}.H7", f"{N.label} receives another new message from Bea", bool(got), platform, expected_on_base=True,
                     commit_seen=bool(committed), waited_s=round(time.time() - linked_at))
        t2 = f"{self.canary}-{prefix}-sent-by-new-device"
        mid2 = self.send_retrying(N, self.ch, t2, 120)
        last = {}
        def bea_sees():
            for m in Bea.messages(self.ch) or []:
                if mid2 and m.get("id") == mid2:
                    last["m"] = m
                    if readable(m):
                        return m
        m = Bea.wait_until("bea sees", bea_sees, 90) if mid2 else None
        seen_as = last.get("m") or {}
        sender = (m or {}).get("sender") or {}
        self.L.check(f"{prefix}.H8", f"Bea reads {N.label}'s message as from Ada (one Ada: same person id)", bool(m) and sender.get("id") == A.user_id, platform,
                     expected_on_base=True, sent=bool(mid2), listed_on_bea=bool(seen_as), seal=seen_as.get("seal"), sender=seen_as.get("sender"),
                     send_failures=[f for f in self.send_failures if f["device"] == N.name][-2:])
        # The same history once newer messages are there (Bea's in the DM, the new device's own in the
        # channel), scrolled up to the start: what a person sees opening each conversation.
        res = self.read_history(N, seconds=60)
        self.L.check(f"{prefix}.H3", f"{N.label} reads the pre-link history, scrolling up (all {len(self.history)} messages, edit, reply, picture)", res["ok"], platform,
                     expected_on_base=False, waiting_on="client: archive export/import (e2ee::archive) + v1 link", **res["why"])
        ordering = []
        for conv in (self.dm, self.ch):
            expected = [h["id"] for h in self.history if h["conv"] == conv]
            displayed = [m["id"] for m in (N.messages(conv) or []) if m.get("id") in expected]
            ordering.append({"conversation": conv, "expected": expected, "displayed": displayed, "ok": expected == displayed})
        self.L.check(f"{prefix}.H3b", f"{N.label} shows imported messages in their original chronological order",
                     all(o["ok"] for o in ordering), platform, expected_on_base=False, ordering=ordering)
        pictures = [verify_picture(N, res["seen"].get(h["id"], {}), self.pics[h["tag"]])
                    for h in self.history if h["kind"] == "picture"]
        self.L.check(f"{prefix}.H4", f"{N.label} downloads each pre-link picture with the original SHA-256 and length",
                     all(p["ok"] for p in pictures), platform, expected_on_base=False, pictures=pictures)
        self.linker.shot(N, platform, f"{prefix}-history-scrolled")
        bea_after = Bea.e2ee({"op": "person", "user_id": A.user_id})[1]
        n_before = len(find_devices(bea_view))
        n_after = len(find_devices(bea_after))
        self.L.check(f"{prefix}.H8b", f"Bea's view of Ada: one person, {n_after} devices (was {n_before})", n_after == n_before + 1 or n_after >= n_before, platform,
                     expected_on_base=True, safety_before=find(bea_view, r"\d{5}( \d{5}){2,}"), safety_after=find(bea_after, r"\d{5}( \d{5}){2,}"))
        # "New device added" (E2eeStatus.added): shown on Ada's OTHER devices when the person's
        # device list changes, not on the device that approved (client feat/device-link 750be14d).
        watchers = [d for d in self.linked if d is not N]
        if self.mode != "v1":
            self.L.check(f"{prefix}.H9", "'New device added' notice", None, platform, why="v1 only")
        elif not watchers:
            self.L.check(f"{prefix}.H9", "'New device added' notice", None, platform, why="no other device of Ada besides the approver yet")
        else:
            nid = (N.e2ee_status() or {}).get("device_id", "")
            def noticed(d):
                added = (d.e2ee_status() or {}).get("added")
                return added if added and (nid in json.dumps(added) or N.label in json.dumps(added)) else None
            seen = {d.name: d.wait_until("notice", lambda d=d: noticed(d), 30) for d in watchers}
            self.L.check(f"{prefix}.H9", f"Ada's other devices show 'New device added' ({', '.join(d.label for d in watchers)})", all(seen.values()), platform,
                         expected_on_base=False, added={k: v for k, v in seen.items()}, new_device=nid)
        self.shot_all(platform, f"{prefix}-after")

    def revoke(self, A, Bea, victim, others, platform="windows"):
        """Ada A revokes `victim`. Bea then sends twice: 3 s after the revoke (what a person would
        hope is already safe) and 30 s after (time for every device to hear of it). The revoked
        device must read neither; H10a and H10b say which held, so lag and a real leak differ."""
        st = victim.e2ee_status() or {}
        vid = st.get("device_id")
        rc, ans = A.e2ee({"op": "revoke", "device_id": vid})
        self.L.step(platform, f"Ada A revokes {victim.label}", rc == 0, answer=ans if rc else None)
        sent = {}
        for tag, delay in (("H10a", 3), ("H10b", 30)):
            time.sleep(delay if tag == "H10a" else delay - 3)
            sent[tag] = self.send_retrying(Bea, self.dm, f"{self.canary}-after-revoke-{delay}s from Bea", 60)
        for d in others:
            ok = d.wait_until("others read", lambda: any(m.get("id") == sent["H10b"] and readable(m) for m in (d.messages(self.dm) or [])), 60)
            self.L.check(f"R.{d.name}", f"{d.label} still reads new messages after the revoke", bool(ok), d.platform if d.platform != "windows" else platform, expected_on_base=True)
        time.sleep(8)
        ms = {m.get("id"): m for m in (victim.messages(self.dm) or [])}
        for tag, delay in (("H10a", 3), ("H10b", 30)):
            m = ms.get(sent[tag])
            self.L.check(tag, f"the revoked {victim.label} cannot read what Bea sends {delay}s after the revoke", not (m and readable(m)), platform,
                         expected_on_base=True, victim_state=ready_state(victim)[0], seal=(m or {}).get("seal"), listed=bool(m))
        self.linker.shot(victim, platform, "revoked")

    # -- negatives --------------------------------------------------------------------------------
    def negative(self, A, case):
        """One negative case on a fresh device (Ada's spare session, a new scratch dir each time)."""
        platform = "windows"
        spare = dict(self.seed["sessions"]["a_spare"], base_url=f"http://localhost:{self.server['proxy_port']}")
        N = WinLocal(f"a_spare_{case}", spare, self.a.app_dir, DLCTL, STATE, f"Ada spare ({case})", gui=not self.a.no_gui)
        self.devices[N.name] = N
        if self.mode != "v1":
            self.L.check(f"N.{case}", f"negative case {case}", None, platform, why="needs the v1 link flow (server does not advertise e2ee_device_link)",
                         waiting_on="server + client v1")
            return
        before = len(((A.e2ee_status() or {}).get("devices") or []))
        N.start()
        expect = {"mitm": "abort-mismatch", "reject": "abort", "tamper": "abort", "expired": "refused-expired", "reuse": "success"}[case]
        if case == "mitm":
            self.linker.proxy(swap_old_pub=True)
        if case == "tamper":
            self.linker.proxy(tamper_archive=True, tamper_offset=100)   # inside the ciphertext of even a small archive
        seen = self.linker.link(A, N, platform, expect=expect, reject=(case == "reject"))
        self.linker.proxy()
        json.dump(seen, open(os.path.join(self.out, platform, f"neg-{case}-link.json"), "w"), indent=1)
        after = len(((A.e2ee_status() or {}).get("devices") or []))
        state = ready_state(N)[0]
        if case == "reuse":
            # link once (it works), then approve the same code again from Ada A: must fail
            rc, ans = A.ipc(fill(self.linker.ops["v1"]["old_join"], {"account": A.user_id, "code": seen.get("code", "")}))
            later = A.wait_until("reuse fails", lambda: (lambda r: r if r.get("stage") == "failed" else None)(
                (A.ipc(fill(self.linker.ops["v1"]["state"], {"account": A.user_id}))[1] or {}).get("link") or {}), 30)
            self.L.check("N.reuse", "a second approval of a used link fails", rc != 0 or bool(later), platform,
                         expected_on_base=False, answer=ans, state=later)
        elif case == "tamper":
            res = self.read_history(N, seconds=20)
            dl = [r for r in seen.get("wire", []) if r.get("route") == "archive" and r.get("method") == "GET"]
            self.L.check("N.tamper", "a tampered archive is rejected: nothing imported, link aborted", bool(dl) and not res["ok"] and res["why"]["unreadable"] + res["why"]["missing"] == len(self.history),
                         platform, expected_on_base=False, archive_gets=dl, state=state)
        else:
            title = {"mitm": "a key swapped by the server (MITM) changes the check code and the link aborts",
                     "reject": "the person says the codes differ: the link aborts",
                     "expired": "an expired link cannot be approved"}[case]
            if case == "mitm":
                differ = seen.get("check_old") != seen.get("check_new")
                self.L.step(platform, f"check codes differ under the swapped key ({seen.get('check_old')} / {seen.get('check_new')})", differ)
            self.L.check(f"N.{case}", title, after == before and state != "ready", platform, expected_on_base=False,
                         devices_before=before, devices_after=after, new_state=state)
        deleted = any(r.get("method") == "DELETE" for r in seen.get("wire", []))
        self.L.check(f"N.{case}.cleanup", "the link is deleted on the server afterwards", deleted or case == "expired", platform, expected_on_base=False)
        N.stop()

    def extended(self, A):
        """Later protocol phases: local encrypted backup and sibling history recovery."""
        import tempfile
        root = os.path.join(tempfile.gettempdir(), "tiny-dl-extended-" + self.run_id)
        os.makedirs(root, exist_ok=False)
        original = os.path.join(root, "history.tinybackup")
        damaged = os.path.join(root, "damaged.tinybackup")
        passphrase = "invented proof phrase " + self.run_id
        rc, answer = A.e2ee({"op": "export_backup", "path": original, "passphrase": passphrase}, timeout=120)
        exported = rc == 0 and os.path.isfile(original) and isinstance(answer, dict) and answer.get("messages", 0) > 0
        self.L.check("X.backup.export", "Ada exports a local encrypted history backup", exported, answer=answer)
        if not exported:
            return
        with open(original, "rb") as file:
            data = bytearray(file.read())
        data[-1] ^= 1
        with open(damaged, "wb") as file:
            file.write(data)
        for name, purpose in (("backup", "backup import"), ("gapfill", "same-user gap fill")):
            spare = dict(self.seed["sessions"]["a_spare"], base_url=f"http://localhost:{self.server['proxy_port']}")
            N = WinLocal("a_" + name, spare, self.a.app_dir, DLCTL, STATE, "Ada " + purpose, gui=not self.a.no_gui)
            self.devices[N.name] = N
            if not N.start():
                self.L.step("windows", purpose + " device starts", False)
                continue
            self.grab_windows()
            seen = self.linker.link(A, N, "windows", history="none")
            self.L.check("X." + name + ".empty-link", "the fresh device links with no history in its archive",
                         seen.get("new_final", {}).get("stage") == "done" and seen.get("new_final", {}).get("messages", 0) == 0,
                         new_final=seen.get("new_final"))
            if name == "backup":
                for case, path, phrase in (("wrong-passphrase", original, "wrong " + passphrase), ("tampered", damaged, passphrase)):
                    rc, answer = N.e2ee({"op": "import_backup", "path": path, "passphrase": phrase}, timeout=120)
                    self.L.check("X.backup." + case, "backup import refuses " + case, rc != 0, answer=answer)
                rc, answer = N.e2ee({"op": "import_backup", "path": original, "passphrase": passphrase}, timeout=120)
                self.L.check("X.backup.import", "the fresh device imports new messages from the authentic backup with the right passphrase",
                             rc == 0 and isinstance(answer, dict) and answer.get("messages", 0) > 0, answer=answer)
            recovered = self.read_history(N, seconds=90)
            self.L.check("X." + name + ".history", "the fresh device reads every original message through " + purpose,
                         recovered["ok"], **recovered["why"])
            pictures = [verify_picture(N, recovered["seen"].get(h["id"], {}), self.pics[h["tag"]])
                        for h in self.history if h["kind"] == "picture"]
            self.L.check("X." + name + ".pictures", purpose + " recovers usable picture keys and original bytes",
                         all(p["ok"] for p in pictures), pictures=pictures)
            N.stop()

    # -- run --------------------------------------------------------------------------------------
    def run(self):
        a = self.a
        self.server_up()
        meta = {"run_id": self.run_id, "started": datetime.datetime.now(datetime.timezone.utc).isoformat(), "canary": self.canary,
                "app_dir": a.app_dir, "app_version": app_version(a.app_dir), "server": {k: self.server.get(k) for k in ("tinyserver_branch", "tinyserver_sha", "e2ee_device_link", "link_ttl_seconds")},
                "mode": self.mode, "history_per_conversation": a.history}
        print(json.dumps(meta))
        A = self.dev("a_desk_a", "Ada A", gui=not a.no_gui)
        Bea = self.dev("b_desk", "Bea", gui=not a.no_gui)
        try:
            for d in (A, Bea):
                ok = d.start() and d.wait_until("ready", lambda: ready_state(d)[0] == "ready", 60)
                self.L.step("windows", f"{d.label} signed in, encryption ready", bool(ok))
                if not ok:
                    raise RuntimeError(f"{d.label} not ready: {ready_state(d)[0]}")
            Bdev = self.dev("a_desk_b", "Ada B", gui=not a.no_gui) if "desk_b" in a.devices else None
            self.start_grabber()
            self.shot_all("windows", "00-start")
            self.seed_history(A, Bea)
            for d in (A, Bea):
                self.open_dm_on_screen(d)
            time.sleep(2)
            self.shot_all("windows", "01-history")
            linked = []
            if Bdev:
                self.link_and_prove(A, Bea, Bdev, "windows", "B")
                linked.append(Bdev)
            if "linux" in a.devices:
                C = self.dev("a_linux", "Ada C (Linux)", where="linux")
                C.record_start()
                self.L.step("linux", "recorder on (x11grab :0)", True)
                self.link_and_prove(A, Bea, C, "linux", "C-linux")
                linked.append(C)
            if "winvm" in a.devices:
                W = self.dev("a_win", "Ada C (Windows VM)", where="winvm")
                self.link_and_prove(A, Bea, W, "windows-vm", "C-win")
                linked.append(W)
            if Bdev and a.revoke:
                self.revoke(A, Bea, Bdev, [A] + [d for d in linked if d is not Bdev])
            for case in a.cases:
                self.negative(A, case)
            if a.extended:
                self.extended(A)
            if "linux" in a.devices:
                self.devices["a_linux"].record_stop(os.path.join(self.out, "linux", "recording-raw.mp4"))
        except Exception as e:
            import traceback
            traceback.print_exc()
            self.L.step("windows", f"driver error: {type(e).__name__}: {e}", False)
        finally:
            self.stop_grabber()
            for d in self.devices.values():
                try:
                    d.collect(os.path.join(self.out, d.platform, "logs", d.name))
                except Exception as e:
                    print(f"collect {d.name}: {e}")
                d.stop()
        # server side: canary check (always last: everything that could leak has happened)
        secrets = [s for s in self.linker.secrets if s]
        json.dump({"canary": self.canary, "link_codes": secrets}, open(os.path.join(self.out, "canary-inputs.json"), "w"))
        r = subprocess.run([sys.executable, os.path.join(HERE, "negative", "canary_check.py"), "--state", STATE,
                            "--inputs", os.path.join(self.out, "canary-inputs.json"), "--out", self.out], capture_output=True, text=True)
        print(r.stdout[-1500:])
        cc = json.load(open(os.path.join(self.out, "canary-check.json"))) if os.path.exists(os.path.join(self.out, "canary-check.json")) else {}
        self.L.check("N.canary", "server DB, logs, media dir and wire hold no message plaintext, picture bytes or link secret", cc.get("ok"), "server",
                     expected_on_base=True, hits=cc.get("hits", [])[:5], searched=cc.get("searched"))
        self.summarize(meta)

    def summarize(self, meta):
        A = self.L.assertions
        decided = [x for x in A if x["ok"] is not None]
        failed = [x for x in decided if not x["ok"]]
        # the red half: on a base build exactly the device-link assertions fail
        base_like = all((x["ok"] == x.get("expected_on_base", x["ok"])) for x in decided)
        # A failed step that is not an assertion (the driver could not get a device up, say) means the
        # run proves nothing either way.
        broken = [f for f in self.L.failed_steps if not re.match(r"^[A-Z][\w.-]*: ", f["step"])]
        if broken:
            verdict = "incomplete"
        else:
            verdict = "pass" if decided and not failed else ("base-as-expected" if base_like and failed else "fail")
        platforms = sorted({x["platform"] for x in A})
        summary = dict(meta, finished=datetime.datetime.now(datetime.timezone.utc).isoformat(), verdict=verdict,
                       failed_steps=self.L.failed_steps,
                       counts={"assertions": len(A), "passed": sum(1 for x in decided if x["ok"]), "failed": len(failed), "skipped": len(A) - len(decided)},
                       platforms={p: {"passed": sum(1 for x in decided if x["platform"] == p and x["ok"]),
                                      "failed": [x["id"] for x in failed if x["platform"] == p]} for p in platforms},
                       assertions=A)
        with open(os.path.join(self.out, "summary.json"), "w", encoding="utf-8") as f:
            json.dump(summary, f, indent=1)
        lines = [f"# device-link proof {self.run_id}: {verdict}", "",
                 f"server `{meta['server']['tinyserver_branch']}@{(meta['server']['tinyserver_sha'] or '')[:9]}`, app {meta['app_version']}, link mode `{self.mode}`", "",
                 "| id | platform | result | title |", "|---|---|---|---|"]
        for x in A:
            r = "skip" if x["ok"] is None else ("PASS" if x["ok"] else "FAIL")
            if x["ok"] is False and x.get("expected_on_base") is False:
                r += " (also fails on base)"
            lines.append(f"| {x['id']} | {x['platform']} | {r} | {x['title']} |")
        open(os.path.join(self.out, "summary.md"), "w", encoding="utf-8").write("\n".join(lines) + "\n")
        print("\n".join(lines))
        print(f"\nevidence: {self.out}")


def find_devices(view):
    if not isinstance(view, dict):
        return []
    stack = [view]
    while stack:
        v = stack.pop()
        if isinstance(v, dict):
            if isinstance(v.get("devices"), list):
                return v["devices"]
            stack.extend(v.values())
    return []


def app_version(app_dir):
    exe = os.path.join(app_dir, "tinyclient.exe")
    try:
        return subprocess.run([exe, "--version"], capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return "?"


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--app-dir", default=INSTALLED, help="folder with tinyclient.exe, chatd.exe, mediad.exe (default: the installed app)")
    r.add_argument("--mode", default="auto", choices=["auto", "legacy", "v1"], help="auto: v1 when the server advertises e2ee_device_link")
    r.add_argument("--devices", default="desk_b", help="comma list: desk_b,linux,winvm")
    r.add_argument("--cases", default="", help="comma list of negative cases: mitm,reject,tamper,expired,reuse")
    r.add_argument("--history", type=int, default=6, help="text messages per conversation before linking (plus edit, reply, picture)")
    r.add_argument("--no-reset", dest="reset", action="store_false", help="reuse the running server (default: fresh DB + seed)")
    r.add_argument("--server-dir", default="", help="tinyserver checkout (default: sibling tinyserver checkout)")
    r.add_argument("--server-ref", default=None, help="commit/branch of that checkout to build (default feat/device-link; '' = the checkout as it is)")
    r.add_argument("--link-ttl", type=int, default=0, help="short link TTL in seconds for the `expired` case")
    r.add_argument("--no-revoke", dest="revoke", action="store_false")
    r.add_argument("--no-gui", action="store_true", help="chatd only, no windows (no screenshots)")
    r.add_argument("--no-record", action="store_true")
    r.add_argument("--out", default="")
    r.add_argument("--extended", action="store_true", help="also prove local backup rejection/roundtrip and same-user gap fill")
    sub.add_parser("link-ops")
    a = p.parse_args()
    if a.cmd == "link-ops":
        print(open(os.path.join(HERE, "link-ops.json")).read())
        return
    a.devices = [x for x in a.devices.split(",") if x]
    a.cases = [x for x in a.cases.split(",") if x]
    if not a.devices or set(a.devices) - {"desk_b", "linux"}:
        p.error("--devices must name desk_b and/or linux")
    if set(a.cases) - {"reject", "mitm", "tamper", "expired", "reuse"}:
        p.error("unknown negative case")
    if not 1 <= a.history <= 50:
        p.error("--history must be in 1..50")
    if a.server_ref == "":
        p.error("--server-ref must name a committed ref")
    scenario = Scenario(a)
    scenario.run()
    summary = json.load(open(os.path.join(scenario.out, "summary.json"), encoding="utf-8"))
    return 0 if summary["verdict"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
