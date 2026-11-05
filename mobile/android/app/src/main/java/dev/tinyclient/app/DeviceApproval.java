package dev.tinyclient.app;

import android.app.Activity;
import android.app.KeyguardManager;
import android.hardware.biometrics.BiometricManager;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Build;
import android.os.CancellationSignal;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;

/** One native owner check per linking attempt. Rust waits on its worker, never on Android UI. */
public final class DeviceApproval {
    private static final class Attempt {
        final AtomicInteger result = new AtomicInteger(0); // pending, verified=1, unconfigured=2, refused=-1
        final CancellationSignal cancellation = new CancellationSignal();
    }
    private static final ConcurrentHashMap<Long, Attempt> attempts = new ConcurrentHashMap<>();
    private DeviceApproval() {}

    public static void start(long id, String reason) {
        Attempt attempt = new Attempt();
        attempts.put(id, attempt);
        TinyPlatform.ui().post(() -> {
            Activity activity = TinyPlatform.activity();
            if (attempts.get(id) != attempt || activity == null || activity.isFinishing() || !activity.hasWindowFocus()) {
                attempt.result.set(-1);
                return;
            }
            try {
                KeyguardManager keyguard = activity.getSystemService(KeyguardManager.class);
                BiometricManager manager = activity.getSystemService(BiometricManager.class);
                if (keyguard == null || manager == null) { attempt.result.set(-1); return; }
                boolean secure = keyguard.isDeviceSecure();
                int availability;
                BiometricPrompt.Builder builder = new BiometricPrompt.Builder(activity)
                    .setTitle("Add a device to Tiny")
                    .setDescription(reason);
                if (Build.VERSION.SDK_INT >= 30) {
                    int authenticators = BiometricManager.Authenticators.BIOMETRIC_STRONG | BiometricManager.Authenticators.DEVICE_CREDENTIAL;
                    availability = manager.canAuthenticate(authenticators);
                    builder.setAllowedAuthenticators(authenticators);
                } else {
                    // API 29 is our minimum. Credential fallback uses this platform API there.
                    availability = manager.canAuthenticate();
                    builder.setDeviceCredentialAllowed(true);
                }
                if (!secure && (availability == BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED
                        || availability == BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE)) {
                    attempt.result.set(2);
                    return;
                }
                if (Build.VERSION.SDK_INT >= 30 && availability != BiometricManager.BIOMETRIC_SUCCESS) {
                    attempt.result.set(-1); // busy, policy, security update and unknown errors never skip
                    return;
                }
                builder.build().authenticate(attempt.cancellation, activity.getMainExecutor(), new BiometricPrompt.AuthenticationCallback() {
                    @Override public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                        attempt.result.compareAndSet(0, 1);
                    }
                    @Override public void onAuthenticationError(int code, CharSequence message) {
                        attempt.result.compareAndSet(0, -1);
                    }
                    // An unsuccessful biometric sample is not a terminal result; the system
                    // controls retries and credential fallback in the same native prompt.
                });
            } catch (RuntimeException failure) {
                attempt.result.compareAndSet(0, -1);
            }
        });
    }

    public static int status(long id) {
        Attempt attempt = attempts.get(id);
        return attempt == null ? -1 : attempt.result.get();
    }

    public static void cancel(long id) {
        Attempt attempt = attempts.remove(id);
        if (attempt != null) {
            attempt.result.compareAndSet(0, -1);
            TinyPlatform.ui().post(attempt.cancellation::cancel);
        }
    }

    static void cancelAll() {
        for (Long id : attempts.keySet()) { cancel(id); }
    }
}
