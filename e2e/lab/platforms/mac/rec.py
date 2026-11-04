#!/usr/bin/env python3
"""Runs `xcrun simctl io <udid> recordVideo` on a pseudo-terminal and says when it started.

simctl buffers its "Recording started" line unless it has a terminal, and it writes the video only
when told to stop, so the lab needs both: the pty makes the line arrive at once, and SIGINT/SIGTERM
sent to this process is passed on as the SIGINT simctl wants.

  rec.py <udid> <out.mp4> <log>
"""
import os
import pty
import select
import signal
import sys

udid, out, logpath = sys.argv[1:4]
pid, fd = pty.fork()
if pid == 0:
    os.execvp("xcrun", ["xcrun", "simctl", "io", udid, "recordVideo", "--codec=h264", "--force", out])

stop = []
signal.signal(signal.SIGINT, lambda *_: stop.append(1))
signal.signal(signal.SIGTERM, lambda *_: stop.append(1))
log = open(logpath, "a", buffering=1)
sent = False
status = None
while True:
    if stop and not sent:
        os.kill(pid, signal.SIGINT)
        sent = True
    try:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            data = os.read(fd, 4096)
            if not data:
                break
            log.write(data.decode("utf-8", "replace").replace("\r", ""))
    except OSError:
        break
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
        break
if status is None:
    _, status = os.waitpid(pid, 0)
log.write("rec.py: simctl exited %d\n" % os.waitstatus_to_exitcode(status))
sys.exit(0 if os.waitstatus_to_exitcode(status) == 0 else 1)
