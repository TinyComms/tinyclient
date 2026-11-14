# Enhanced noise turns the room down to a floor, not to nothing

Run with `E2E_APP_DIR` pointing to a directory containing the built `mediad`
and `E2E_OUT` pointing to an evidence directory, then run `python drive.py`.

This is a **headless DSP regression**, not native window or phone UI proof.
It starts the real mediad process with `TINYCLIENT_PROVE=isolation-bounded`;
that build runs `mediad::process::prove_isolation_limit` and exits 0 only
when both hold, with Enhanced chosen the way a call sets it:

- Three seconds of steady hiss come down by the isolation's limit
  (`ISOLATION_LIMIT_DB`, within 2 dB, after the 3.1 dB WebRTC's high-pass
  takes off white noise on its own) and stay there. Without the limit the
  model takes the hiss down by more than 40 dB, which between words is a room
  gated to silence and let back in warbling.
- A synthetic voice (harmonics on a wobbling pitch, gated like syllables)
  loses no more than the limit. Without it the model takes tens of decibels
  off anything it doubts is a voice, the ends of words included.

With the base there is no limit and no proof hook: the variable is ignored,
mediad opens the audio engine and stays up, and the driver fails at its
45 second deadline. With this branch it exits zero in a few seconds, no
devices opened.

The same assertions run as
`cargo test -p mediad --lib the_isolation_proof_holds`, and
`isolation_is_held_to_its_limit` holds the limit on a voice in a room, but
this scenario proves the shipped binary carries the limit, the way the lab
can run it on every desktop platform.
