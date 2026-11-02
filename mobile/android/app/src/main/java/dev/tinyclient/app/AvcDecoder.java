package dev.tinyclient.app;

import android.graphics.Rect;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.util.Log;

import java.nio.ByteBuffer;

/**
 * An H.264 decoder on MediaCodec, used synchronously from one Rust thread: Annex B access units
 * in, the newest picture out as I420 behind an 8-byte header (width, height, big-endian).
 */
public final class AvcDecoder {
    private static final String AVC = MediaFormat.MIMETYPE_VIDEO_AVC;

    private final MediaCodec codec;
    private final MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
    private long pictures;
    private boolean keyed;

    private AvcDecoder(MediaCodec codec) {
        this.codec = codec;
    }

    public static AvcDecoder create() {
        MediaCodec codec = null;
        try {
            codec = MediaCodec.createDecoderByType(AVC);
            MediaFormat format = MediaFormat.createVideoFormat(AVC, 1280, 720);
            format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            codec.configure(format, null, null, 0);
            codec.start();
            Log.i(TinyPlatform.TAG, "H.264 decoder " + codec.getName());
            return new AvcDecoder(codec);
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "H.264 decoder refused: " + e);
            if (codec != null) {
                codec.release();
            }
            return null;
        }
    }

    /** Header + I420 of the newest picture out, an empty array for none yet, null on failure. */
    public byte[] decode(byte[] annexB, long timestampUs) {
        try {
            if (!keyed) {
                // A decoder fed P-frames before its first SPS/IDR only complains; wait for one.
                if (!java.util.Arrays.asList(AvcEncoder.nalTypes(annexB).split(",")).contains("7")) {
                    return new byte[0];
                }
                keyed = true;
            }
            int in = codec.dequeueInputBuffer(20_000);
            if (in >= 0) {
                ByteBuffer buffer = codec.getInputBuffer(in);
                buffer.clear();
                buffer.put(annexB);
                codec.queueInputBuffer(in, 0, annexB.length, timestampUs, 0);
            }
            byte[] newest = new byte[0];
            long wait = 20_000;
            while (true) {
                int index = codec.dequeueOutputBuffer(info, wait);
                wait = 0;
                if (index == MediaCodec.INFO_TRY_AGAIN_LATER) {
                    break;
                }
                if (index < 0) {
                    continue;
                }
                Image image = codec.getOutputImage(index);
                if (image != null) {
                    Rect crop = image.getCropRect();
                    int w = crop.width() & ~1;
                    int h = crop.height() & ~1;
                    byte[] yuv = Yuv.i420(image, crop.left, crop.top, w, h);
                    byte[] out = new byte[8 + yuv.length];
                    ByteBuffer.wrap(out).putInt(w).putInt(h);
                    System.arraycopy(yuv, 0, out, 8, yuv.length);
                    newest = out;
                    image.close();
                    if (pictures++ == 0) {
                        Log.i(TinyPlatform.TAG, "H.264 decoder first picture " + w + "x" + h);
                    }
                }
                codec.releaseOutputBuffer(index, false);
            }
            return newest;
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "H.264 decode", e);
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
}
