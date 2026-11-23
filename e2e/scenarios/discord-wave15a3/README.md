# Discord video codecs, camera simulcast, and multi-stream watch (Wave 15A3)

The driver starts this build's `chatd`, then `mediad`, both with `TINYCLIENT_PROVE=discord-wave15a3`.

- `chatd` verifies that multiple Go Live streams are tracked concurrently in the Discord adapter's watch map, that unwatching one stream preserves the others, and that per-owner `StreamStop` round-trips cleanly.
- `mediad` verifies that Select Protocol offers H.264 (encode+decode), VP8 (decode), and AV1 (decode), that camera op 12 announces high (`rid` `100`, 720p@30) and low (`rid` `50`, 360p@15) simulcast layers, that op 15 emits per-SSRC sink wants scaled by tile size/focus, and that the AV1 RTP depacketizer + pure-Rust `rusty_av1d` decoder decode a genuine AV1 keyframe into 64×48 I420 planes.

A build from before Wave 15A3 ignores `TINYCLIENT_PROVE=discord-wave15a3` and stays running, so the driver fails there.
