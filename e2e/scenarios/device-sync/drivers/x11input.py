#!/usr/bin/env python3
"""Keyboard and mouse input for the smoke driver on Linux/X11, through XTest (libX11 + libXtst, which
the desktop already has), so the lab needs no xdotool. Input is real: the window manager and the
app see it as a person's.

  x11input.py key ctrl+q            one combination (modifiers: ctrl alt shift super), then a key name
  x11input.py key Down Return       several, 0.15 s apart
  x11input.py click X Y             move and left-click
  x11input.py type some words       type text
"""
import ctypes
import sys
import time

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]

MODS = {"ctrl": "Control_L", "alt": "Alt_L", "shift": "Shift_L", "super": "Super_L"}
dpy = x11.XOpenDisplay(None)
if not dpy:
    sys.exit("x11input: cannot open the display (DISPLAY/XAUTHORITY?)")


def code(name):
    sym = x11.XStringToKeysym(name.encode())
    if not sym:
        sys.exit("x11input: no key called %r" % name)
    kc = x11.XKeysymToKeycode(dpy, sym)
    if not kc:
        sys.exit("x11input: %r is not on this keyboard" % name)
    return kc


def press(names):
    codes = [code(n) for n in names]
    for c in codes:
        xtst.XTestFakeKeyEvent(dpy, c, 1, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.03)
    for c in reversed(codes):
        xtst.XTestFakeKeyEvent(dpy, c, 0, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.03)


def combo(text):
    parts = text.split("+")
    press([MODS.get(p.lower(), p) for p in parts[:-1]] + [parts[-1]])


def main():
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "key":
        for i, a in enumerate(args):
            combo(a)
            if i + 1 < len(args):
                time.sleep(0.15)
    elif cmd == "type":
        for ch in " ".join(args):
            combo({" ": "space"}.get(ch, ch)) if not ch.isupper() else press(["Shift_L", ch])
            time.sleep(0.05)
    elif cmd == "click":
        x, y = int(args[0]), int(args[1])
        xtst.XTestFakeMotionEvent(dpy, -1, x, y, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.1)
        xtst.XTestFakeButtonEvent(dpy, 1, 1, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.05)
        xtst.XTestFakeButtonEvent(dpy, 1, 0, 0)
    else:
        sys.exit(__doc__)
    x11.XSync(dpy, 0)


main()
