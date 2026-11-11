# Hardware encoder fallback

Run `sh e2e/scenarios/hardware-encoder-fallback/run.sh`. It needs no devices, no encoder
hardware and no network: the scenario drives the tests of
`crates/mediad/src/mf_encoder.rs` and the fallback seam in `picture/desktop.rs` — the
encoder selection, the fake-MFT seam that holds the open (the H.264 output type refused,
a driver that kept Main/High or named no profile, a keyframe that only counts when the
sample says `MFSampleExtension_CleanPoint`, the frame loop with its asks, a driver that
goes silent called hung after two seconds so the failover runs), the pure NV12
and Annex B helpers, and the place a refusal of the hardware becomes the OpenH264
fallback.

On the base the scenario fails: the module does not exist, so cargo reports no matching
tests. With the branch the Media Foundation hardware encoder is tried first (the output
type negotiated first, at the constrained baseline, and what the driver kept read back
and held to that), and the OpenH264 runtime download stays the fallback with its own
covered path.

The on-hardware record (the lab driving a real share over a call) needs a machine with a
hardware MFT; that recording is the released-client proof this scenario points to. What
this scenario can say without that machine: a driver that would not keep the promised
wire is refused in its own words and OpenH264 takes the job, and nothing else changed
about what goes out.
