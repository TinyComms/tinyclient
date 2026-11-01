package dev.tinyclient.app;

import android.media.Image;

import java.nio.ByteBuffer;

/** YUV_420_888 images to tightly packed NV12 and I420, and NV12 rotation. */
final class Yuv {
    private Yuv() {}

    private static byte[] bytes(ByteBuffer buffer) {
        ByteBuffer b = buffer.duplicate();
        b.rewind();
        byte[] out = new byte[b.remaining()];
        b.get(out);
        return out;
    }

    /**
     * The crop {@code (left, top, w, h)} of a YUV_420_888 image as NV12 (Y, then CbCr pairs),
     * no row padding. {@code w} and {@code h} must be even.
     */
    static byte[] nv12(Image image, int left, int top, int w, int h) {
        Image.Plane[] planes = image.getPlanes();
        byte[] out = new byte[w * h * 3 / 2];
        byte[] y = bytes(planes[0].getBuffer());
        int yRow = planes[0].getRowStride();
        int yPix = planes[0].getPixelStride();
        int o = 0;
        for (int r = 0; r < h; r++) {
            int base = (top + r) * yRow + left * yPix;
            if (yPix == 1) {
                System.arraycopy(y, base, out, o, w);
                o += w;
            } else {
                for (int c = 0; c < w; c++) {
                    out[o++] = y[base + c * yPix];
                }
            }
        }
        byte[] u = bytes(planes[1].getBuffer());
        byte[] v = bytes(planes[2].getBuffer());
        int uRow = planes[1].getRowStride();
        int uPix = planes[1].getPixelStride();
        int vRow = planes[2].getRowStride();
        int vPix = planes[2].getPixelStride();
        for (int r = 0; r < h / 2; r++) {
            int ub = (top / 2 + r) * uRow + (left / 2) * uPix;
            int vb = (top / 2 + r) * vRow + (left / 2) * vPix;
            for (int c = 0; c < w / 2; c++) {
                out[o++] = u[ub + c * uPix];
                out[o++] = v[vb + c * vPix];
            }
        }
        return out;
    }

    /** The same crop as I420 (Y, U, V planes). */
    static byte[] i420(Image image, int left, int top, int w, int h) {
        byte[] nv = nv12(image, left, top, w, h);
        byte[] out = new byte[nv.length];
        int ySize = w * h;
        int q = ySize / 4;
        System.arraycopy(nv, 0, out, 0, ySize);
        for (int i = 0; i < q; i++) {
            out[ySize + i] = nv[ySize + 2 * i];
            out[ySize + q + i] = nv[ySize + 2 * i + 1];
        }
        return out;
    }

    /** NV12 {@code w}×{@code h} turned clockwise by {@code degrees} (0, 90, 180, 270). */
    static byte[] rotate(byte[] in, int w, int h, int degrees) {
        if (degrees == 0) {
            return in;
        }
        byte[] out = new byte[in.length];
        int ySize = w * h;
        if (degrees == 180) {
            for (int i = 0; i < ySize; i++) {
                out[i] = in[ySize - 1 - i];
            }
            int pairs = ySize / 4;
            for (int i = 0; i < pairs; i++) {
                out[ySize + 2 * i] = in[ySize + 2 * (pairs - 1 - i)];
                out[ySize + 2 * i + 1] = in[ySize + 2 * (pairs - 1 - i) + 1];
            }
            return out;
        }
        // 90 clockwise: the new picture is h wide, w tall; new (x, y) = old (y', x') with
        // old row = h - 1 - x for 90, old col = w - 1 - y for 270.
        boolean cw = degrees == 90;
        int nw = h;
        int o = 0;
        for (int ny = 0; ny < w; ny++) {
            for (int nx = 0; nx < nw; nx++) {
                int ox = cw ? ny : w - 1 - ny;
                int oy = cw ? h - 1 - nx : nx;
                out[o++] = in[oy * w + ox];
            }
        }
        int cw2 = w / 2;
        int ch2 = h / 2;
        for (int ny = 0; ny < cw2; ny++) {
            for (int nx = 0; nx < ch2; nx++) {
                int ox = cw ? ny : cw2 - 1 - ny;
                int oy = cw ? ch2 - 1 - nx : nx;
                int src = ySize + 2 * (oy * cw2 + ox);
                out[o++] = in[src];
                out[o++] = in[src + 1];
            }
        }
        return out;
    }
}
