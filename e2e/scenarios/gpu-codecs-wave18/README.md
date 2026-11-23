# Desktop hardware video codecs

Uses the [Tiny video probe](../tiny-video-received/README.md) fixture and the packaged
release `mediad`. Each pass sends a moving share and camera pattern over encrypted room
and direct calls, checks the receiver's JPEGs, and reads encoder and decoder names from
both clients' live call stats. The recording labels each leg with the reported names.

The first pass uses automatic probing. Set `TINY_GPU_EXPECT_ENCODER` and
`TINY_GPU_EXPECT_DECODER` on a GPU-equipped lab runner to require its hardware backends.
Use `Media Foundation` and `Media Foundation (D3D11)` on Windows, `VideoToolbox` on macOS,
and `VA-API` or `NVENC` for Linux encoding and `VA-API` or `NVDEC` for Linux decoding. With no
expectations set, automatic mode also covers a runner that falls back to OpenH264.

The second pass disables hardware in the isolated lab profiles and requires OpenH264
in both directions. Both passes require actual decoded motion.
