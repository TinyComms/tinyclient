package dev.tinyclient.app;

import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Bundle;
import android.util.Log;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;

/**
 * An H.264 encoder on MediaCodec, used synchronously from one Rust thread: NV12 in, Annex B
 * out, constrained baseline, SPS and PPS in front of every IDR.
 */
public final class AvcEncoder {
    private static final String AVC = MediaFormat.MIMETYPE_VIDEO_AVC;
    private static final long INPUT_WAIT_US = 20_000;
    private static final long FIRST_OUTPUT_WAIT_US = 30_000;

    private final MediaCodec codec;
    private final int width;
    private final int height;
    private final MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
    private byte[] config = new byte[0];
    private final java.util.ArrayDeque<byte[]> ready = new java.util.ArrayDeque<>();
    private long frames;
    private long nals;

    private AvcEncoder(MediaCodec codec, int width, int height) {
        this.codec = codec;
        this.width = width;
        this.height = height;
    }

    /** A started encoder, or null when no H.264 encoder takes this configuration. */
    public static AvcEncoder create(int width, int height, int bitrate, int fps, boolean screen) {
        int cb = MediaCodecInfo.CodecProfileLevel.AVCProfileConstrainedBaseline;
        int base = MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline;
        // From the most asked to the plainest: a codec that refuses a key (Codec2's software
        // encoder refuses the real-time ones) gets the next. SPS/PPS go in front of each IDR
        // here when the codec does not prepend them (encode); B-frames are off by default in
        // baseline.
        int[][] tries = {{cb, 1}, {cb, 0}, {base, 0}, {-1, 0}};
        for (int[] attempt : tries) {
            int profile = attempt[0];
            boolean realtime = attempt[1] == 1;
            MediaCodec codec = null;
            try {
                codec = MediaCodec.createEncoderByType(AVC);
                MediaFormat format = MediaFormat.createVideoFormat(AVC, width, height);
                format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
                format.setInteger(MediaFormat.KEY_BIT_RATE, bitrate);
                format.setInteger(MediaFormat.KEY_FRAME_RATE, fps);
                format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, screen ? 10 : 4);
                if (realtime) {
                    format.setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1);
                    format.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0);
                    format.setInteger(MediaFormat.KEY_LATENCY, 1);
                    format.setInteger(MediaFormat.KEY_PRIORITY, 0);
                }
                if (profile >= 0) {
                    format.setInteger(MediaFormat.KEY_PROFILE, profile);
                    format.setInteger(MediaFormat.KEY_LEVEL, MediaCodecInfo.CodecProfileLevel.AVCLevel31);
                }
                if (screen) {
                    format.setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR);
                }
                codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                codec.start();
                Log.i(TinyPlatform.TAG, "H.264 encoder " + codec.getName() + " " + width + "x" + height + " " + bitrate + " bit/s, profile " + profile + (realtime ? ", real-time keys" : ""));
                return new AvcEncoder(codec, width, height);
            } catch (Exception e) {
                Log.i(TinyPlatform.TAG, "H.264 encoder refused profile " + profile + (realtime ? " with real-time keys" : "") + ": " + e);
                if (codec != null) {
                    codec.release();
                }
            }
        }
        return null;
    }

    /** One NV12 frame in; Annex B out (empty while the codec holds it), or null on failure. */
    public byte[] encode(byte[] nv12, long timestampUs, boolean keyframe) {
        try {
            if (keyframe) {
                Bundle sync = new Bundle();
                sync.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
                codec.setParameters(sync);
            }
            int in = codec.dequeueInputBuffer(INPUT_WAIT_US);
            if (in >= 0) {
                Image image = codec.getInputImage(in);
                if (image != null) {
                    fill(image, nv12);
                } else {
                    ByteBuffer buffer = codec.getInputBuffer(in);
                    buffer.clear();
                    buffer.put(nv12, 0, Math.min(nv12.length, buffer.remaining()));
                }
                codec.queueInputBuffer(in, 0, nv12.length, timestampUs, 0);
                frames++;
            }
            // Everything the codec has done goes to `ready`, one picture each; one is answered per
            // call, so a codec that falls a frame behind still sends one picture a tick.
            long wait = ready.isEmpty() ? FIRST_OUTPUT_WAIT_US : 0;
            while (true) {
                int index = codec.dequeueOutputBuffer(info, wait);
                wait = 0;
                if (index == MediaCodec.INFO_TRY_AGAIN_LATER) {
                    break;
                }
                if (index < 0) {
                    continue;
                }
                ByteBuffer buffer = codec.getOutputBuffer(index);
                byte[] chunk = new byte[info.size];
                buffer.position(info.offset);
                buffer.get(chunk, 0, info.size);
                boolean isConfig = (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
                boolean isKey = (info.flags & MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0;
                codec.releaseOutputBuffer(index, false);
                if (isConfig) {
                    config = chunk;
                    continue;
                }
                if (isKey && config.length > 0 && !startsWithSps(chunk)) {
                    byte[] whole = new byte[config.length + chunk.length];
                    System.arraycopy(config, 0, whole, 0, config.length);
                    System.arraycopy(chunk, 0, whole, config.length, chunk.length);
                    chunk = whole;
                }
                if (nals++ < 3 || isKey) {
                    Log.i(TinyPlatform.TAG, "H.264 encoder " + width + "x" + height + " out " + chunk.length + " bytes, NAL types " + nalTypes(chunk) + (isKey ? " (IDR)" : ""));
                }
                ready.add(chunk);
            }
            // Never more than a few behind: dropping a picture would break every one after it
            // until the next keyframe, so a long backlog goes out at once instead.
            if (ready.size() > 4) {
                ByteArrayOutputStream all = new ByteArrayOutputStream();
                for (byte[] picture; (picture = ready.poll()) != null; ) {
                    all.write(picture, 0, picture.length);
                }
                return all.toByteArray();
            }
            byte[] next = ready.poll();
            return next == null ? new byte[0] : next;
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "H.264 encode", e);
            return null;
        }
    }

    public void release() {
        try {
            codec.stop();
        } catch (Exception ignored) {
        }
        codec.release();
    }

    private void fill(Image image, byte[] nv12) {
        Image.Plane[] planes = image.getPlanes();
        ByteBuffer y = planes[0].getBuffer();
        int yRow = planes[0].getRowStride();
        int yPix = planes[0].getPixelStride();
        for (int r = 0; r < height; r++) {
            if (yPix == 1) {
                y.position(r * yRow);
                y.put(nv12, r * width, width);
            } else {
                for (int c = 0; c < width; c++) {
                    y.put(r * yRow + c * yPix, nv12[r * width + c]);
                }
            }
        }
        ByteBuffer u = planes[1].getBuffer();
        ByteBuffer v = planes[2].getBuffer();
        int uRow = planes[1].getRowStride();
        int uPix = planes[1].getPixelStride();
        int vRow = planes[2].getRowStride();
        int vPix = planes[2].getPixelStride();
        int base = width * height;
        if (uPix == 1 && vPix == 1) {
            // Planar (what Codec2's software encoder asks for): split each CbCr row, put it whole.
            int half = width / 2;
            byte[] rowU = new byte[half];
            byte[] rowV = new byte[half];
            for (int r = 0; r < height / 2; r++) {
                int src = base + r * width;
                for (int c = 0; c < half; c++) {
                    rowU[c] = nv12[src + 2 * c];
                    rowV[c] = nv12[src + 2 * c + 1];
                }
                u.position(r * uRow);
                u.put(rowU, 0, half);
                v.position(r * vRow);
                v.put(rowV, 0, half);
            }
            return;
        }
        for (int r = 0; r < height / 2; r++) {
            for (int c = 0; c < width / 2; c++) {
                int src = base + r * width + 2 * c;
                u.put(r * uRow + c * uPix, nv12[src]);
                v.put(r * vRow + c * vPix, nv12[src + 1]);
            }
        }
    }

    private static boolean startsWithSps(byte[] data) {
        int at = data.length > 4 && data[2] == 1 ? 3 : 4;
        return data.length > at && (data[at] & 0x1f) == 7;
    }

    /** The NAL unit types in an Annex B buffer, for the log. */
    static String nalTypes(byte[] data) {
        StringBuilder types = new StringBuilder();
        for (int i = 0; i + 3 < data.length; i++) {
            if (data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1) {
                if (types.length() > 0) {
                    types.append(',');
                }
                types.append(data[i + 3] & 0x1f);
                i += 3;
            }
        }
        return types.toString();
    }
}
