# Enhanced noise keeps the high band, joined at the path's lag

Run with `E2E_APP_DIR` pointing to a directory containing the built `mediad`
and `E2E_OUT` pointing to an evidence directory, then run `python drive.py`.

This is a **headless DSP regression**, not native window or phone UI proof.
It starts the real mediad process with `TINYCLIENT_PROVE=high-band`; that
build runs `mediad::process::prove_high_band` and exits 0 only when both
hold:

- An impulse's high band (the high-pass's response) lands with its peak at
  `impulse + RECON_DELAY`, to the sample - the ring that delays the band
  above the 7.2 kHz cut hands over exactly `DELAY`, not a whole frame.
- The sparkle above the cut rides the send at exactly the isolation's own gain,
  whatever the model decides about the synthetic voice under it: front, voice
  isolation, the bridge, the rejoin, and back. The high band follows the
  isolation's gain, so it keeps what a voice keeps and loses what the model
  silences. The lag itself is pinned by the impulse check; a full-path lag
  measurement is not asserted, because the model attenuates synthetic
  speech-like signals by tens of decibels and the correlation peak rides the
  residue.

With base `7ffa840` there is no rejoin and no proof hook: the variable is
ignored, mediad opens the audio engine and stays up, and the driver fails.
With the intermediate rejoin that read the ring slot it wrote, the high band
came back 457 samples (9.5 ms) late and the lag step fails. With this branch
it exits zero in about a second, no devices opened.

The same assertions run as
`cargo test -p mediad --lib the_high_band_joins_at_the_path_delay`, but this
scenario proves the shipped binary carries the proof hook, the way the lab
can run it on every desktop platform.
