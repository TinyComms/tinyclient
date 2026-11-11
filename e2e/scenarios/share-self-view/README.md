# My own share is painted for me

Sharing a screen in a Tiny call used to leave my own stage with a text card and a blurred, stale
thumbnail from the start of the share. The share now paints a small live picture of what it sends,
the way the camera already did, and the stage draws that with Stop over it.

This headless scenario runs the real capture thread (`mediad::video::Video`) on the real screen of
the host: it asks for a share, waits for the first picture painted for my own tile, and checks it
is a JPEG the window can read, no bigger than the camera's corner size, and that no camera
picture is made while the camera is off. It then stops the share and requires the capture thread to
say the picture is gone. The host needs a screen and the OpenH264 binary (fetched on first use), as
`the_capture_thread_shares_a_real_screen_through_the_real_encoder` does.

Run `python e2e/scenarios/share-self-view/drive.py`. It requires the one ignored test and both
`share-self-view-proof:` lines, so a build from before the change (which has no such test) fails.

This proves the production half on the platform where it runs. It does not open a window: the
stage drawing that picture, Stop over it, and the fallback card are covered by the window's own
tests, and the Tiny call that carries `share_view` from mediad through chatd is covered by chatd's
`media::tests`.
