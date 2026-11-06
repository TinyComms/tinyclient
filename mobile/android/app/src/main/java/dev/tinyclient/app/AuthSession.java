package dev.tinyclient.app;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.Color;
import android.net.Uri;
import android.os.Looper;
import android.util.Log;

import androidx.browser.customtabs.CustomTabColorSchemeParams;
import androidx.browser.customtabs.CustomTabsCallback;
import androidx.browser.customtabs.CustomTabsClient;
import androidx.browser.customtabs.CustomTabsIntent;
import androidx.browser.customtabs.CustomTabsService;
import androidx.browser.customtabs.CustomTabsServiceConnection;
import androidx.browser.customtabs.CustomTabsSession;

import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * OIDC in a browser Custom Tab. A real session authorizes Chromium's KEEP_ALIVE binding back
 * to our empty service while the tab is foreground, so Android cannot freeze chatd's loopback
 * listener. The browser releases that binding when its tab stops; Tiny releases its outgoing
 * connection when sign-in ends. No started service or foreground notification is needed.
 */
public final class AuthSession {
    private static final String EXTRA_KEEP_ALIVE = "android.support.customtabs.extra.KEEP_ALIVE";
    private static final long CONNECT_MILLIS = 3000;
    // Match chatd's five-minute sign-in patience; never keep an abandoned auth session alive.
    private static final long AUTH_MILLIS = 5 * 60 * 1000;
    private static volatile AuthSession current;

    final long id;
    final String url;
    final String callback;
    private final Context context;
    private final CountDownLatch ready = new CountDownLatch(1);
    private final Runnable connectExpired = () -> fail(false);
    private final Runnable authExpired = () -> fail(true);
    private final CustomTabsServiceConnection connection;
    private volatile boolean launched;
    private volatile boolean leftApp;
    volatile boolean closed;
    private volatile boolean finished;
    // Accessed only on the UI thread, along with the browser/service lifecycle callbacks.
    private boolean bound;
    private boolean released;

    private AuthSession(long id, String url, String callback, Context context) {
        this.id = id;
        this.url = url;
        this.callback = callback;
        this.context = context.getApplicationContext();
        connection = new CustomTabsServiceConnection() {
            @Override public void onCustomTabsServiceConnected(ComponentName name, CustomTabsClient client) {
                if (finished) return;
                // newSession is synchronous Binder IPC. Keep it off the UI thread so its
                // registration deadline and cancellation can run even if the browser stalls.
                Thread registration = new Thread(() -> register(client), "tiny-auth-registration");
                registration.setDaemon(true);
                registration.start();
            }
            @Override public void onServiceDisconnected(ComponentName name) { fail(true); }
            @Override public void onBindingDied(ComponentName name) { fail(true); }
            @Override public void onNullBinding(ComponentName name) { fail(false); }
        };
    }

    private void register(CustomTabsClient client) {
        if (finished) return;
        final CustomTabsSession browserSession;
        try {
            browserSession = client.newSession(new CustomTabsCallback());
        } catch (RuntimeException error) {
            TinyPlatform.ui().post(() -> { if (!finished) fail(false); });
            return;
        }
        TinyPlatform.ui().post(() -> {
                // A deadline, replacement or cancellation may have won while Binder waited.
                if (finished) return;
                if (browserSession == null) {
                    fail(false);
                    return;
                }
                CustomTabsIntent tab = new CustomTabsIntent.Builder(browserSession)
                        .setDefaultColorSchemeParams(new CustomTabColorSchemeParams.Builder()
                                .setToolbarColor(Color.rgb(24, 24, 27)).build())
                        .build();
                tab.intent.putExtra(EXTRA_KEEP_ALIVE, new Intent(AuthSession.this.context, AuthKeepAliveService.class));
                Activity activity = TinyPlatform.activity();
                Context launchContext = activity != null ? activity : AuthSession.this.context;
                if (activity == null) tab.intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                try {
                    launched = true;
                    tab.launchUrl(launchContext, Uri.parse(url));
                    TinyPlatform.ui().removeCallbacks(connectExpired);
                    ready.countDown();
                    Log.i(TinyPlatform.TAG, "Tiny sign-in opened with a browser session");
                } catch (RuntimeException e) {
                    launched = false;
                    Log.w(TinyPlatform.TAG, "opening the sign-in tab", e);
                    fail(false);
                }
        });
    }

    /** Called from Rust's background thread; waits only for browser registration, never auth. */
    public static AuthSession open(long id, String url, String callback) {
        Context context = TinyPlatform.context();
        if (context == null) return null;
        String browser = customTabsBrowser(context);
        if (browser == null) return null;
        AuthSession session = new AuthSession(id, url, callback, context);
        Boolean binding = TinyPlatform.onUi(CONNECT_MILLIS, () -> {
            if (session.finished) return false;
            AuthSession previous = current;
            if (previous != null) previous.release(false);
            current = session;
            TinyPlatform.ui().postDelayed(session.connectExpired, CONNECT_MILLIS);
            TinyPlatform.ui().postDelayed(session.authExpired, AUTH_MILLIS);
            try {
                session.bound = CustomTabsClient.bindCustomTabsService(session.context, browser, session.connection);
                if (!session.bound) session.fail(false);
                return session.bound;
            } catch (RuntimeException e) {
                Log.w(TinyPlatform.TAG, "connecting to the sign-in browser", e);
                session.fail(false);
                return false;
            }
        });
        if (!Boolean.TRUE.equals(binding)) {
            session.close();
            return null;
        }
        // Never block the UI callback that registers the session. Production callers are Rust
        // workers; an Android UI caller receives a pending handle and can observe isClosed().
        if (Looper.myLooper() != Looper.getMainLooper()) {
            try {
                session.ready.await(CONNECT_MILLIS, TimeUnit.MILLISECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            if (!session.launched || session.finished) {
                session.close();
                return null;
            }
        }
        return session;
    }

    /** Chrome first, then another browser offering the real Custom Tabs session protocol. */
    private static String customTabsBrowser(Context context) {
        PackageManager pm = context.getPackageManager();
        List<ResolveInfo> services = pm.queryIntentServices(new Intent(CustomTabsService.ACTION_CUSTOM_TABS_CONNECTION), 0);
        String any = null;
        for (ResolveInfo info : services) {
            String pkg = info.serviceInfo.packageName;
            if ("com.android.chrome".equals(pkg)) return pkg;
            if (any == null) any = pkg;
        }
        return any;
    }

    /** The exported service has no data or callable operations and accepts only active auth. */
    static boolean needsKeepAlive() {
        AuthSession session = current;
        return session != null && !session.finished;
    }

    static void onActivityPaused() {
        AuthSession session = current;
        if (session != null && session.launched) session.leftApp = true;
    }

    static void onActivityResumed() {
        AuthSession session = current;
        if (session != null && session.leftApp && !session.finished) {
            session.closed = true;
            session.release(false);
            Log.i(TinyPlatform.TAG, "Tiny sign-in tab left");
        }
    }

    public boolean isClosed() { return closed; }

    private void fail(boolean bringBack) {
        closed = true;
        release(bringBack);
    }

    /** UI-thread cleanup. Returning to Tiny stops Chrome's tab and its incoming binding. */
    private void release(boolean bringBack) {
        if (released) return;
        released = true;
        finished = true;
        if (current == this) current = null;
        TinyPlatform.ui().removeCallbacks(connectExpired);
        TinyPlatform.ui().removeCallbacks(authExpired);
        ready.countDown();
        if (bound) {
            bound = false;
            context.unbindService(connection);
        }
        if (bringBack && launched) {
            Activity activity = TinyPlatform.activity();
            Context target = activity != null ? activity : context;
            Intent back = new Intent(target, TinyActivity.class)
                    .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            if (activity == null) back.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                target.startActivity(back);
            } catch (RuntimeException e) {
                Log.w(TinyPlatform.TAG, "bringing the app back", e);
            }
        }
    }

    /** Sign-in has ended (success, server error, cancel or timeout); safe to call repeatedly. */
    public void close() {
        finished = true; // Prevent a queued/late service callback from launching another tab.
        boolean bringBack = !closed;
        if (Looper.myLooper() == Looper.getMainLooper()) release(bringBack);
        else TinyPlatform.ui().post(() -> release(bringBack));
    }
}
