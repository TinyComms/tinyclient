package dev.tinyclient.app;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;
import android.util.Log;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Callable;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * What Rust's call platform and web sign-in ask of Android (crates/tinyclient/src/mobile/android).
 *
 * <p>Every static method here is called from a Rust thread (mediad's call loop, a chatd request
 * thread), never the UI thread, and may block it: each one hops to the UI thread itself when an
 * API needs it. See docs/mobile-platform.md for the contract.
 */
public final class TinyPlatform {
    static final String TAG = "tinyclient";

    private static volatile TinyActivity activity;
    private static volatile Context app;
    private static final Handler ui = new Handler(Looper.getMainLooper());

    private TinyPlatform() {}

    // ---- the activity -------------------------------------------------------------------

    static void attach(TinyActivity a) {
        activity = a;
        app = a.getApplicationContext();
    }

    static void detach(TinyActivity a) {
        if (activity == a) {
            activity = null;
        }
    }

    static Activity activity() {
        return activity;
    }

    static Context context() {
        Activity a = activity;
        return a != null ? a : app;
    }

    static Handler ui() {
        return ui;
    }

    /** Runs {@code work} on the UI thread and waits up to {@code millis} for its answer. */
    static <T> T onUi(long millis, Callable<T> work) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            try {
                return work.call();
            } catch (Exception e) {
                Log.w(TAG, "on the UI thread", e);
                return null;
            }
        }
        AtomicReference<T> answer = new AtomicReference<>();
        CountDownLatch done = new CountDownLatch(1);
        ui.post(() -> {
            try {
                answer.set(work.call());
            } catch (Exception e) {
                Log.w(TAG, "on the UI thread", e);
            } finally {
                done.countDown();
            }
        });
        try {
            done.await(millis, TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        return answer.get();
    }

    // ---- permissions --------------------------------------------------------------------

    /** 0 microphone, 1 camera. */
    private static final int REQUEST_PERMISSIONS = 0x7101;
    private static final Object asking = new Object();
    private static volatile CountDownLatch answered;
    private static volatile boolean granted;

    /**
     * Asks for the microphone (0) or the camera (1) and blocks until the user answers.
     * Returns 0 granted, 1 refused, 2 refused for good (the system no longer asks; Settings
     * only), 3 no activity to ask from.
     */
    public static int requestPermission(int kind) {
        String wanted = kind == 0 ? Manifest.permission.RECORD_AUDIO : Manifest.permission.CAMERA;
        Context c = context();
        if (c != null && c.checkSelfPermission(wanted) == PackageManager.PERMISSION_GRANTED) {
            return 0;
        }
        synchronized (asking) {
            Activity a = activity;
            if (a == null) {
                return 3;
            }
            List<String> ask = new ArrayList<>();
            ask.add(wanted);
            // A call's notification is shown by its foreground service; on 33+ it needs this.
            if (kind == 0 && Build.VERSION.SDK_INT >= 33
                    && a.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                ask.add(Manifest.permission.POST_NOTIFICATIONS);
            }
            if (kind == 0 && Build.VERSION.SDK_INT >= 31
                    && a.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) != PackageManager.PERMISSION_GRANTED) {
                ask.add(Manifest.permission.BLUETOOTH_CONNECT);
            }
            CountDownLatch latch = new CountDownLatch(1);
            answered = latch;
            granted = false;
            Log.i(TAG, "asking for " + ask);
            ui.post(() -> a.requestPermissions(ask.toArray(new String[0]), REQUEST_PERMISSIONS));
            try {
                // The dialog waits for the user; ten minutes is "never answered".
                if (!latch.await(10, TimeUnit.MINUTES)) {
                    return 1;
                }
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return 1;
            }
            answered = null;
            if (granted) {
                return 0;
            }
            Boolean rationale = onUi(2000, () -> a.shouldShowRequestPermissionRationale(wanted));
            return Boolean.TRUE.equals(rationale) ? 1 : 2;
        }
    }

    static void onPermissionsResult(int requestCode, String[] permissions, int[] results) {
        if (requestCode != REQUEST_PERMISSIONS) {
            return;
        }
        boolean ok = false;
        for (int i = 0; i < permissions.length && i < results.length; i++) {
            Log.i(TAG, permissions[i] + (results[i] == PackageManager.PERMISSION_GRANTED ? " granted" : " refused"));
            if ((Manifest.permission.RECORD_AUDIO.equals(permissions[i]) || Manifest.permission.CAMERA.equals(permissions[i]))
                    && results[i] == PackageManager.PERMISSION_GRANTED) {
                ok = true;
            }
        }
        granted = ok;
        CountDownLatch latch = answered;
        if (latch != null) {
            latch.countDown();
        }
    }

    /** True when the microphone (0) or camera (1) is allowed now, without asking. */
    public static boolean hasPermission(int kind) {
        Context c = context();
        String wanted = kind == 0 ? Manifest.permission.RECORD_AUDIO : Manifest.permission.CAMERA;
        return c != null && c.checkSelfPermission(wanted) == PackageManager.PERMISSION_GRANTED;
    }

    /** Opens this app's page of the system settings, where its permissions are allowed. */
    public static void openSettings() {
        Context c = context();
        if (c == null) {
            return;
        }
        Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", c.getPackageName(), null));
        if (!(c instanceof Activity)) {
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        ui.post(() -> {
            try {
                c.startActivity(intent);
            } catch (Exception e) {
                Log.w(TAG, "no settings page", e);
            }
        });
    }

    // ---- results only an activity receives ---------------------------------------------

    static void onActivityResult(int requestCode, int resultCode, Intent data) {
        ScreenCapture.onActivityResult(requestCode, resultCode, data);
    }

    // ---- natives (crates/tinyclient/src/mobile/android/natives.rs) ------------------------

    /** A captured frame for the Rust sink {@code sink}: format 1 NV12, 2 RGBA. */
    static native void nativeFrame(long sink, int width, int height, int format, byte[] data, long timestampUs);

    /** A web page's navigation: "A" allow, "C" cancel, "L" + url load that instead. */
    static native String nativeNavigate(long page, String url);

    /** The Tiny sign-in's browser reached the callback with {@code url}. */
    static native void nativeRedirected(long auth, String url);
}
