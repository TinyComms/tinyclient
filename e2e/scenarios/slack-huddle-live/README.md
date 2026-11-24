# Slack huddle live

The desktop driver starts a fresh profile against the loopback Slack workspace in `fixture.py`:
one channel, `standup`. Once the app is signed in, the driver has the workspace say on its socket
that a huddle started, the way Slack posts the huddle's message, with Ana and Ben in it. It then
checks, in this order:

1. the channel's row says who is in the huddle, before the conversation is opened: hover it, and
   it reads "Ana and Ben are in this huddle";
2. the header offers "Join in Slack" and counts the two: "Join in Slack · 2";
3. the bar under the header names the huddle and the two people: "Huddle in progress · Ana, Ben";
4. Ben leaves (Slack updates the same message) and the header counts one;
5. the huddle ends and nothing on screen offers joining it any more, while its card reads
   "Huddle ended".

From the row's hover on, each step needs the change and is expected to fail on the base build,
which knows of no huddle being live and keeps the quiet "Huddle" pill.

Two things are drawn that the driver cannot read, and it says so here rather than claiming them.
The row's headphones glyph is drawn from the same occupants as its hover text and the header's
pill, and a glyph is not something OCR reads, so the hover text is what stands for the row. The
bar's two faces are not read either; its line names the two people instead.

The drivers use PyAutoGUI and Tesseract. The driver never presses Join, which would hand the
huddle to Slack itself. The fixture data is invented. The scenario does not join a call or use a
Slack login.
