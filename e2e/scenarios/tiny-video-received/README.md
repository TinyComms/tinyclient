# Tiny received video

Two independent chatd live adapters and their real mediad processes use the same disposable
server fixture as [tiny-direct-calls](../tiny-direct-calls/README.md). Use fresh synthetic owner
and peer sessions. The owner creates and deletes a video-enabled room named Proof and then rings
the peer in a direct. Both peers must report end-to-end security in each call.

The sender replaces the screen and camera with the same changing color and 16-bit frame counter.
The probe requires an encode session and local preview, the remote participant's share or camera
flag, a decode session, a valid JPEG from the sender, ten distinct painted generations and an
advancing counter within five seconds, a cleared picture within five seconds of stopping, and
first-keyframe evidence in the viewer's mediad log. It uses Tiny's native Watch request, which is
the route for multi-video SFUs. The headless peer supplies the selection a shell would make for
a direct call. [call-video-seen](../call-video-seen/README.md) exercises the shell's auto-watch.

Run the platform driver with `TINY_DIRECT_CALL_FIXTURE` pointing to the private fixture JSON.
For lab runs, put this fixture at `~/.config/tiny-lab/direct-call-fixture.json` on each desktop
under the lab OS user. Use a distinct pair per host so simultaneous live sign-ins cannot replace
one another. The lab supplies `E2E_SRC_DIR` and `E2E_APP_DIR` from the exact commit under test.
Without a prebuilt engine the probe builds its matching release mediad. The driver checks that
one test passed and that its proof marker is present. The recorded evidence is a playback of
actual received JPEGs, labeled by call and source. It is not a GUI recording.

Run again with `TINY_VIDEO_SKIP_ROOM_WATCH=1` to require a failure at the first room-share JPEG,
or with `TINY_VIDEO_FREEZE_PATTERN=1` to require failure at the motion assertion. Use a fresh
server fixture for each run. Keep fixture JSON outside the repository and evidence folders.
