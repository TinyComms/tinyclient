# Slack huddle summary, thread, and notes

The desktop driver starts a fresh profile against the loopback Slack workspace. It checks the live Join action and the ended huddle's starter, participant count, duration, and notes preview. It opens the thread from the ended card and checks for an actual reply in the side pane. It then follows the notes card through the link confirmation to a local page standing in for Slack Canvas.

The summary check requires the new card and is expected to fail on the base build. The drivers use PyAutoGUI, Tesseract, and the lab's default browser. The fixture data is invented. The scenario does not join a call or use a Slack login.
