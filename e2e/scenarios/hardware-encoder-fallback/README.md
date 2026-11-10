# Hardware encoder fallback

Run `sh e2e/scenarios/hardware-encoder-fallback/run.sh`. It needs no devices and no
network: the scenario drives the selection logic and the pure NV12/Annex B helpers of
`crates/mediad/src/mf_encoder.rs`.

On the base the scenario fails: the module does not exist, so cargo reports no matching
tests. With the branch the Media Foundation hardware encoder is tried first (when the
machine lists one that takes NV12 and answers H.264) and the OpenH264 runtime download
stays the fallback with its own covered path.

The on-hardware record (the lab driving a real share over a call) needs a machine with a
hardware MFT; that recording is the released-client proof this scenario points to.
