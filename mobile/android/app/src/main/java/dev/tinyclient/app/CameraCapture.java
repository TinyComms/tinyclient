package dev.tinyclient.app;

import android.annotation.SuppressLint;
import android.content.Context;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.util.Log;
import android.util.Range;
import android.util.Size;
import android.view.Surface;
import android.view.WindowManager;

import java.util.Collections;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * The call's camera: Camera2, YUV_420_888 through an ImageReader, turned upright and handed to
 * the Rust sink as NV12 at about the asked rate. One camera at a time; {@link #start} replaces
 * a running one, {@link #flip} turns to the other side with the same sink.
 */
public final class CameraCapture {
    private static HandlerThread thread;
    private static Handler handler;
    private static CameraDevice device;
    private static CameraCaptureSession session;
    private static ImageReader reader;
    private static long sink;
    private static boolean front = true;
    private static int width = 640;
    private static int height = 360;
    private static int fps = 15;
    private static long lastFrameMs;
    private static int rotation;
    private static long frames;

    private CameraCapture() {}

    /** Starts the front (or back) camera for {@code sinkId}; blocks until frames come or it fails. */
    public static synchronized String start(boolean wantFront, int w, int h, int rate, long sinkId) {
        stopLocked();
        front = wantFront;
        width = w;
        height = h;
        fps = Math.max(1, rate);
        sink = sinkId;
        return openLocked();
    }

    /** Turns the running camera to the front or the back. */
    public static synchronized String flip(boolean wantFront) {
        if (sink == 0) {
            front = wantFront;
            return null;
        }
        long keep = sink;
        stopLocked();
        sink = keep;
        front = wantFront;
        return openLocked();
    }

    public static synchronized void stop() {
        stopLocked();
        sink = 0;
    }

    private static void stopLocked() {
        long had = sink;
        sink = 0;
        if (session != null) {
            try {
                session.close();
            } catch (Exception ignored) {
            }
            session = null;
        }
        if (device != null) {
            device.close();
            device = null;
        }
        if (reader != null) {
            reader.close();
            reader = null;
        }
        if (thread != null) {
            thread.quitSafely();
            thread = null;
            handler = null;
        }
        if (had != 0) {
            Log.i(TinyPlatform.TAG, "camera stopped after " + frames + " frames");
            CallService.remove(TinyPlatform.context(), CallService.CAMERA);
        }
    }

    @SuppressLint("MissingPermission")
    private static String openLocked() {
        Context c = TinyPlatform.context();
        if (c == null) {
            return "The camera isn't ready yet.";
        }
        if (!TinyPlatform.hasPermission(1)) {
            return "Tiny isn't allowed to use the camera.";
        }
        CameraManager cm = (CameraManager) c.getSystemService(Context.CAMERA_SERVICE);
        String id;
        CameraCharacteristics info;
        try {
            id = pick(cm, front);
            if (id == null) {
                return "This phone has no camera.";
            }
            info = cm.getCameraCharacteristics(id);
        } catch (CameraAccessException e) {
            return "The camera can't be opened right now.";
        }
        Integer facing = info.get(CameraCharacteristics.LENS_FACING);
        boolean isFront = facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT;
        StreamConfigurationMap map = info.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
        Size size = nearest(map.getOutputSizes(ImageFormat.YUV_420_888), width, height);
        Integer sensor = info.get(CameraCharacteristics.SENSOR_ORIENTATION);
        int display = displayDegrees(c);
        int s = sensor == null ? 0 : sensor;
        rotation = isFront ? (s + display) % 360 : (s - display + 360) % 360;
        Range<Integer> range = rate(info.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES), fps);

        CallService.add(c, CallService.CAMERA);
        thread = new HandlerThread("tiny-camera");
        thread.start();
        handler = new Handler(thread.getLooper());
        reader = ImageReader.newInstance(size.getWidth(), size.getHeight(), ImageFormat.YUV_420_888, 3);
        CountDownLatch first = new CountDownLatch(1);
        String[] failed = new String[1];
        frames = 0;
        lastFrameMs = 0;
        long target = sink;
        reader.setOnImageAvailableListener(r -> deliver(r, target, first), handler);
        try {
            cm.openCamera(id, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    device = camera;
                    configure(camera, range, failed, first);
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    camera.close();
                    failed[0] = "The camera was taken by another app.";
                    first.countDown();
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    camera.close();
                    failed[0] = error == ERROR_CAMERA_IN_USE || error == ERROR_MAX_CAMERAS_IN_USE
                            ? "Another app is using the camera." : "The camera stopped with an error.";
                    first.countDown();
                }
            }, handler);
        } catch (CameraAccessException | SecurityException e) {
            Log.w(TinyPlatform.TAG, "openCamera", e);
            stopLocked();
            return "The camera can't be opened right now.";
        }
        try {
            if (!first.await(6, TimeUnit.SECONDS)) {
                failed[0] = "The camera didn't start.";
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        if (failed[0] != null) {
            stopLocked();
            return failed[0];
        }
        Log.i(TinyPlatform.TAG, "camera " + id + (isFront ? " front " : " back ") + size + " rotation " + rotation + " fps " + range);
        return null;
    }

    private static void configure(CameraDevice camera, Range<Integer> range, String[] failed, CountDownLatch first) {
        try {
            Surface surface = reader.getSurface();
            CaptureRequest.Builder request = camera.createCaptureRequest(CameraDevice.TEMPLATE_RECORD);
            request.addTarget(surface);
            if (range != null) {
                request.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range);
            }
            camera.createCaptureSession(Collections.singletonList(surface), new CameraCaptureSession.StateCallback() {
                @Override
                public void onConfigured(CameraCaptureSession s) {
                    session = s;
                    try {
                        s.setRepeatingRequest(request.build(), null, handler);
                    } catch (Exception e) {
                        failed[0] = "The camera didn't start.";
                        first.countDown();
                    }
                }

                @Override
                public void onConfigureFailed(CameraCaptureSession s) {
                    failed[0] = "The camera didn't start.";
                    first.countDown();
                }
            }, handler);
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "camera session", e);
            failed[0] = "The camera didn't start.";
            first.countDown();
        }
    }

    private static void deliver(ImageReader r, long target, CountDownLatch first) {
        Image image = r.acquireLatestImage();
        if (image == null) {
            return;
        }
        try {
            long now = SystemClock.elapsedRealtime();
            if (lastFrameMs != 0 && now - lastFrameMs < 1000 / fps - 5) {
                return;
            }
            lastFrameMs = now;
            int w = image.getWidth() & ~1;
            int h = image.getHeight() & ~1;
            byte[] nv12 = Yuv.rotate(Yuv.nv12(image, 0, 0, w, h), w, h, rotation);
            boolean turned = rotation == 90 || rotation == 270;
            TinyPlatform.nativeFrame(target, turned ? h : w, turned ? w : h, 1, nv12, image.getTimestamp() / 1000);
            if (frames++ == 0) {
                Log.i(TinyPlatform.TAG, "camera first frame " + (turned ? h : w) + "x" + (turned ? w : h));
            }
            first.countDown();
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "camera frame", e);
        } finally {
            image.close();
        }
    }

    private static String pick(CameraManager cm, boolean wantFront) throws CameraAccessException {
        String any = null;
        for (String id : cm.getCameraIdList()) {
            Integer facing = cm.getCameraCharacteristics(id).get(CameraCharacteristics.LENS_FACING);
            if (any == null) {
                any = id;
            }
            if (facing != null && facing == (wantFront ? CameraCharacteristics.LENS_FACING_FRONT : CameraCharacteristics.LENS_FACING_BACK)) {
                return id;
            }
        }
        return any;
    }

    /** The size closest in area to w×h (either orientation), preferring one at least that big. */
    private static Size nearest(Size[] sizes, int w, int h) {
        long want = (long) w * h;
        Size best = sizes[0];
        long bestScore = Long.MAX_VALUE;
        for (Size s : sizes) {
            long area = (long) s.getWidth() * s.getHeight();
            long score = area >= want ? area - want : (want - area) * 4;
            if (score < bestScore) {
                bestScore = score;
                best = s;
            }
        }
        return best;
    }

    private static Range<Integer> rate(Range<Integer>[] ranges, int fps) {
        if (ranges == null || ranges.length == 0) {
            return null;
        }
        Range<Integer> best = ranges[0];
        int bestScore = Integer.MAX_VALUE;
        for (Range<Integer> r : ranges) {
            int score = r.contains(fps) ? r.getUpper() - r.getLower() : 1000 + Math.abs(r.getUpper() - fps);
            if (score < bestScore) {
                bestScore = score;
                best = r;
            }
        }
        return best;
    }

    private static int displayDegrees(Context c) {
        try {
            int r = c.getDisplay().getRotation();
            switch (r) {
                case Surface.ROTATION_90:
                    return 90;
                case Surface.ROTATION_180:
                    return 180;
                case Surface.ROTATION_270:
                    return 270;
                default:
                    return 0;
            }
        } catch (Exception e) {
            WindowManager wm = (WindowManager) c.getSystemService(Context.WINDOW_SERVICE);
            return wm == null ? 0 : wm.getDefaultDisplay().getRotation() * 90;
        }
    }
}
