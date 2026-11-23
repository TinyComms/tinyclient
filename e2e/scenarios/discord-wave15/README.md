# Discord Go Live audio

The desktop driver opens an invented Discord direct call, watches the peer's stream, and checks the
440 Hz tone in mediad's speaker capture. It mutes and unmutes the stream in the focused tile and checks
the same tone disappears and returns. The call and stream use independent loopback voice connections.
The stream fixture announces the owner's audio SSRC using voice opcode 12.

The socket fixture uses the pre-epoch audio path and AES-GCM transport. The mediad integration test
also delivers the stream in a separate DAVE MLS group. The lab needs Python with PyAutoGUI and
cryptography, Tesseract, and FFmpeg with its libopus encoder. The scenario uses a fresh app profile
and writes the captured PCM and screenshots beside its step log.
