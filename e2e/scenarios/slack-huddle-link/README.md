# Slack huddle link

The desktop driver starts a fresh profile against the loopback Slack workspace in `fixture.py`:
one channel with a live huddle, and another whose one message is the huddle's address, as
someone pasted it. It right-clicks the huddle's row, takes "Copy huddle link", and checks that
the clipboard holds `https://app.slack.com/huddle/<team>/<channel>` exactly. It then opens the
other channel, clicks the address in the message, and checks that the huddle's conversation
opens with its "Huddle in progress" card and its Join button. It does not press Join, which
would hand the huddle to Slack itself.

The menu item and the clipboard step need the change and are expected to fail on the base build.
The drivers use PyAutoGUI, Tesseract, and a way to read the clipboard: PowerShell's
`Get-Clipboard` on Windows, `pbpaste` on macOS, `xclip` or `xsel` on Linux. The fixture data is
invented. The scenario does not join a call or use a Slack login.
