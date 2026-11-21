# The sharer's pointer is in the shared frames

A shared screen used to go out without the sharer's pointer on Windows and X11: a plain pixel
grab says nothing about where the pointer is, so viewers could not follow what was being pointed
at. The share now draws the pointer into the frames where the platform can, on by default, and a
share can ask for none (`ShareSend::show_cursor`, additive on the wire: absent is on, so an
older shell reads the same frame).

This headless scenario runs the real capture thread (`mediad::video::Video`) on the real screen
of the host, the way `share-self-view` does. It asks for a share, requires the capture to go out
with the pointer shown (the default the capture config starts on), and requires frames to flow
with it; it then requires a share that asks for none to turn the pointer off. The recording the
lab takes shows the pointer wherever the platform draws it.

Where the pointer comes from, per platform, said plainly:

- Windows: mediad fetches it (`GetCursorInfo`, `GetIconInfo`) and draws it into every frame it
  captures, on the screen through Desktop Duplication, on the screen's GDI fallback, and on a
  shared window.
- Linux, Wayland: the portal is asked for the pointer embedded in the stream buffers, which is
  what it already asked for; a share that asks for none asks the portal for a hidden one.
- Linux, X11: not yet. The X11 root grab draws no pointer, and blending one in
  (`XFixesGetCursorImage`) is stage 2. The flag still reaches mediad, and the scenario passes
  on the plumbing; the recording on an X11 host shows no pointer.
- macOS: not yet. A Core Graphics grab has no pointer and no mode that draws one; it comes with
  the ScreenCaptureKit capture (#773), whose `showsCursor` is the native mode. The flag is kept
  for that day.
- Phones: nothing. There is no pointer.

Run `python e2e/scenarios/share-pointer/drive.py`. It requires the one ignored test and both
`share-pointer-proof:` lines, so a build from before the change (which has no such test) fails.
