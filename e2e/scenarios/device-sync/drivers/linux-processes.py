"""Record exact owned Linux processes and stop them through pidfds, never recycled PIDs."""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import sys
import tempfile
import time


def identity(pid):
    proc = Path("/proc") / str(pid)
    try:
        fields = (proc / "stat").read_text().rsplit(") ", 1)[1].split()
        if fields[0] == "Z":
            return None
        return {"pid": pid, "uid": proc.stat().st_uid, "start_ticks": int(fields[19]),
                "exe": os.readlink(proc / "exe"),
                "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip()}
    except FileNotFoundError:
        return None


def atomic_write(path, value):
    path = Path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        output.write(value)
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def records(path):
    if not path.exists():
        return []
    value = json.loads(path.read_text())
    if value.get("schema") != 1 or not isinstance(value.get("processes"), list):
        raise ValueError("unknown process ownership manifest")
    return value["processes"]


def record(path, legacy, pid, expected_exe):
    expected_exe = str(Path(expected_exe).resolve())
    deadline = time.monotonic() + 3
    while True:
        current = identity(pid)
        if current is None:
            raise ValueError("owned process exited before registration")
        if current["uid"] != os.geteuid():
            raise ValueError("owned process has another UID")
        if current["exe"] == expected_exe:
            break
        if time.monotonic() >= deadline:
            raise ValueError("owned process did not execute the expected binary")
        time.sleep(0.02)
    existing = [item for item in records(path) if item["pid"] != pid]
    existing.append(current)
    atomic_write(path, json.dumps({"schema": 1, "processes": existing}))
    atomic_write(legacy, "".join(str(item["pid"]) + "\n" for item in existing))


def stop(path, legacy, first_signal, timeout):
    expected = records(path)
    known = {item["pid"] for item in expected}
    if legacy.exists():
        for line in legacy.read_text().splitlines():
            pid = int(line)
            if pid not in known and identity(pid) is not None:
                raise ValueError("live legacy PID has no verified ownership record")
    handles = []
    try:
        # Validate the entire set before signalling anything. Open handles bind subsequent
        # signals to these processes even if an exit lets Linux reuse a numeric PID.
        for item in expected:
            if item.get("uid") != os.geteuid():
                raise ValueError("process record belongs to another UID")
            try:
                handle = os.pidfd_open(item["pid"])
            except ProcessLookupError:
                continue
            handles.append(handle)
            current = identity(item["pid"])
            if current is not None and current != item:
                raise ValueError("process identity mismatch; refusing cleanup")
        poller = select.poll()
        for handle in handles:
            poller.register(handle, select.POLLIN)
            try:
                signal.pidfd_send_signal(handle, first_signal)
            except ProcessLookupError:
                pass
        pending = set(handles)
        for duration, following in ((timeout, signal.SIGKILL), (1.0, None)):
            deadline = time.monotonic() + duration
            while pending and time.monotonic() < deadline:
                for handle, _ in poller.poll(max(1, int((deadline - time.monotonic()) * 1000))):
                    if handle in pending:
                        pending.remove(handle)
                        poller.unregister(handle)
            if pending and following is not None:
                for handle in pending:
                    try:
                        signal.pidfd_send_signal(handle, following)
                    except ProcessLookupError:
                        pass
        if pending:
            raise ValueError("owned process did not stop; retaining ownership record")
        path.unlink(missing_ok=True)
        legacy.unlink(missing_ok=True)
    finally:
        for handle in handles:
            os.close(handle)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["record", "stop"])
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--legacy", required=True, type=Path)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--expected-exe")
    parser.add_argument("--signal", choices=["TERM", "INT"], default="TERM")
    parser.add_argument("--timeout", type=float, default=0.5)
    args = parser.parse_args()
    try:
        if args.action == "record":
            if not args.pid or not args.expected_exe:
                raise ValueError("record needs PID and executable")
            record(args.manifest, args.legacy, args.pid, args.expected_exe)
        else:
            if not 0 <= args.timeout <= 30:
                raise ValueError("cleanup timeout is outside its bound")
            stop(args.manifest, args.legacy, getattr(signal, "SIG" + args.signal), args.timeout)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        print("Owned-process guard:", error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
