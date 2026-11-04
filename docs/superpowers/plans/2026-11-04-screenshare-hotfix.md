# Screensharing hotfix

Justin requested investigation, repair, proof and shipping in one session. Keep the existing
H.264/str0m transport and quality presets; repair the broken paths rather than replace the stack.

1. Reproduce decoder recovery after a discontinuity with a regression test. Accept complete
   keyframes after gaps, retain rejection of dependent frames, and test recovery again.
2. Route the desktop sidebar share control through the same picker as the call bar.
3. Make Windows GDI readback follow GetDIBits requirements, validate resources and complete
   readback, and allow Desktop Duplication to obtain its initial image before falling back.
4. Add video counters at successful transport writes, received access units, and completed
   painted images. Publish interval rates and last-image age through backward-compatible stats.
   Explain round-trip latency without inventing one-way or capture-to-display latency.
5. Run targeted and broader media/protocol/GUI checks, inspect actual Windows captures and
   prove a sender/receiver path. Review the diff, open the hotfix PR, require green CI, squash
   merge, run the release cut check, release if eligible, and verify the installed client.

Unrelated open PRs and the dirty shared checkout stay outside this hotfix. A server change or
deployment requires evidence of a server defect and the repo's explicit deployment rules.
