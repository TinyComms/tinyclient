package dev.tinyclient.app;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.graphics.PixelFormat;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.util.DisplayMetrics;
import android.util.Log;

import java.nio.ByteBuffer;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * A screen share: MediaProjection after the system's consent dialog, under the call's
 * foreground service with the mediaProjection type, frames as RGBA to the Rust sink.
 */
public final class ScreenCapture {
    private static final int REQUEST_CONSENT = 0x7103;
    private static final int MAX_EDGE = 1280;
    private static final int FPS = 15;

    private static volatile CountDownLatch consented;
    private static volatile int consentCode;
    private static volatile Intent consentData;

    private static MediaProjection projection;
    private static VirtualDisplay display;
    private static ImageReader reader;
    private static HandlerThread thread;
    private static long sink;
    private static long lastMs;
    private static long frames;

    private ScreenCapture() {}

    static void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != REQUEST_CONSENT) {
            return;
        }
        consentCode = resultCode;
        consentData = data;
        CountDownLatch latch = consented;
        if (latch != null) {
            latch.countDown();
        }
    }

    /** Asks for consent, then shares into {@code sinkId}. Blocks until running or refused. */
    public static synchronized String start(long sinkId) {
        stopLocked();
        Activity a = TinyPlatform.activity();
        if (a == null) {
            return "Sharing needs the app on screen.";
        }
        MediaProjectionManager mpm = (MediaProjectionManager) a.getSystemService(Context.MEDIA_PROJECTION_SERVICE);
        CountDownLatch latch = new CountDownLatch(1);
        consented = latch;
        consentData = null;
        consentCode = Activity.RESULT_CANCELED;
        TinyPlatform.ui().post(() -> {
            try {
                a.startActivityForResult(mpm.createScreenCaptureIntent(), REQUEST_CONSENT);
            } catch (Exception e) {
                Log.w(TinyPlatform.TAG, "screen capture consent", e);
                latch.countDown();
            }
        });
        try {
            latch.await(10, TimeUnit.MINUTES);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        consented = null;
        if (consentCode != Activity.RESULT_OK || consentData == null) {
            Log.i(TinyPlatform.TAG, "screen share refused");
            return "Screen sharing wasn't allowed.";
        }
        // Android 14: the service must hold the mediaProjection type before getMediaProjection.
        CallService.add(a, CallService.PROJECTION);
        try {
            projection = mpm.getMediaProjection(consentCode, consentData);
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "getMediaProjection", e);
            CallService.remove(a, CallService.PROJECTION);
            return "The screen can't be shared right now.";
        }
        if (projection == null) {
            CallService.remove(a, CallService.PROJECTION);
            return "The screen can't be shared right now.";
        }
        DisplayMetrics metrics = a.getResources().getDisplayMetrics();
        int w = metrics.widthPixels;
        int h = metrics.heightPixels;
        float scale = Math.min(1f, (float) MAX_EDGE / Math.max(w, h));
        w = Math.max(2, Math.round(w * scale) & ~1);
        h = Math.max(2, Math.round(h * scale) & ~1);
        thread = new HandlerThread("tiny-screen");
        thread.start();
        Handler handler = new Handler(thread.getLooper());
        sink = sinkId;
        frames = 0;
        lastMs = 0;
        reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 2);
        final long target = sinkId;
        reader.setOnImageAvailableListener(r -> deliver(r, target), handler);
        projection.registerCallback(new MediaProjection.Callback() {
            @Override
            public void onStop() {
                Log.i(TinyPlatform.TAG, "screen share stopped by the system");
                synchronized (ScreenCapture.class) {
                    if (sink == target) {
                        stopLocked();
                    }
                }
            }
        }, handler);
        display = projection.createVirtualDisplay("tiny-share", w, h, metrics.densityDpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, reader.getSurface(), null, handler);
        Log.i(TinyPlatform.TAG, "screen share " + w + "x" + h);
        return null;
    }

    public static synchronized void stop() {
        stopLocked();
    }

    private static void stopLocked() {
        boolean had = projection != null;
        sink = 0;
        if (display != null) {
            display.release();
            display = null;
        }
        if (projection != null) {
            MediaProjection p = projection;
            projection = null;
            p.stop();
        }
        if (reader != null) {
            reader.close();
            reader = null;
        }
        if (thread != null) {
            thread.quitSafely();
            thread = null;
        }
        if (had) {
            Log.i(TinyPlatform.TAG, "screen share ended after " + frames + " frames");
            CallService.remove(TinyPlatform.context(), CallService.PROJECTION);
        }
    }

    private static void deliver(ImageReader r, long target) {
        Image image = r.acquireLatestImage();
        if (image == null) {
            return;
        }
        try {
            long now = SystemClock.elapsedRealtime();
            if (lastMs != 0 && now - lastMs < 1000 / FPS - 5) {
                return;
            }
            lastMs = now;
            int w = image.getWidth();
            int h = image.getHeight();
            Image.Plane plane = image.getPlanes()[0];
            ByteBuffer buffer = plane.getBuffer();
            int row = plane.getRowStride();
            byte[] out = new byte[w * h * 4];
            if (row == w * 4) {
                buffer.get(out, 0, out.length);
            } else {
                for (int y = 0; y < h; y++) {
                    buffer.position(y * row);
                    buffer.get(out, y * w * 4, w * 4);
                }
            }
            TinyPlatform.nativeFrame(target, w, h, 2, out, image.getTimestamp() / 1000);
            if (frames++ == 0) {
                Log.i(TinyPlatform.TAG, "screen share first frame " + w + "x" + h);
            }
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "screen frame", e);
        } finally {
            image.close();
        }
    }
}
