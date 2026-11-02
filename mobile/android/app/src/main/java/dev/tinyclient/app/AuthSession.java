package dev.tinyclient.app;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.Color;
import android.net.Uri;
import android.os.Bundle;
import android.util.Log;

import java.util.List;

/**
 * A Tiny server's OIDC sign-in in a Custom Tab, where passkeys work (Credential Manager). The
 * provider redirects the tab to chatd's loopback listener in this process, which receives it as
 * on a desktop. The tab cannot be closed from outside; {@link #close} brings the app back.
 *
 * <p>The Custom Tabs protocol is used without androidx: an ACTION_VIEW intent carrying the
 * {@code EXTRA_SESSION} binder extra (null) is what {@code CustomTabsIntent} builds.
 */
public final class AuthSession {
    private static final String EXTRA_SESSION = "android.support.customtabs.extra.SESSION";
    private static final String EXTRA_TOOLBAR_COLOR = "android.support.customtabs.extra.TOOLBAR_COLOR";
    private static final String SERVICE_ACTION = "android.support.customtabs.action.CustomTabsService";

    private static volatile AuthSession current;

    final long id;
    final String url;
    final String callback;
    private volatile boolean launched;
    private volatile boolean leftApp;
    volatile boolean closed;
    private volatile boolean finished;

    private AuthSession(long id, String url, String callback) {
        this.id = id;
        this.url = url;
        this.callback = callback;
    }

    /** Shows {@code url} and returns at once; null when nothing can show it. */
    public static AuthSession open(long id, String url, String callback) {
        Activity a = TinyPlatform.activity();
        Context c = a != null ? a : TinyPlatform.context();
        if (c == null) {
            return null;
        }
        AuthSession session = new AuthSession(id, url, callback);
        Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
        Bundle extras = new Bundle();
        extras.putBinder(EXTRA_SESSION, null);
        intent.putExtras(extras);
        intent.putExtra(EXTRA_TOOLBAR_COLOR, Color.rgb(24, 24, 27));
        String browser = customTabsBrowser(c);
        if (browser != null) {
            intent.setPackage(browser);
        }
        if (a == null) {
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        current = session;
        Boolean started = TinyPlatform.onUi(3000, () -> {
            c.startActivity(intent);
            session.launched = true;
            return true;
        });
        if (!Boolean.TRUE.equals(started)) {
            current = null;
            return null;
        }
        Log.i(TinyPlatform.TAG, "Tiny sign-in opened in " + (browser == null ? "the default browser" : browser + "'s Custom Tab"));
        return session;
    }

    /** The browser that offers Custom Tabs: Chrome first, else any. */
    private static String customTabsBrowser(Context c) {
        PackageManager pm = c.getPackageManager();
        List<ResolveInfo> services = pm.queryIntentServices(new Intent(SERVICE_ACTION), 0);
        String any = null;
        for (ResolveInfo info : services) {
            String pkg = info.serviceInfo.packageName;
            if ("com.android.chrome".equals(pkg)) {
                return pkg;
            }
            if (any == null) {
                any = pkg;
            }
        }
        return any;
    }

    /** The app went to the background: the tab is in front. */
    static void onActivityPaused() {
        AuthSession s = current;
        if (s != null && s.launched) {
            s.leftApp = true;
        }
    }

    /** The app came back while the tab was open: the user closed it (or chatd brought us back). */
    static void onActivityResumed() {
        AuthSession s = current;
        if (s != null && s.leftApp && !s.finished) {
            s.closed = true;
            Log.i(TinyPlatform.TAG, "Tiny sign-in tab left");
        }
    }

    public boolean isClosed() {
        return closed;
    }

    /** The sign-in is over: bring the app's window back in front of the tab. */
    public void close() {
        finished = true;
        if (current == this) {
            current = null;
        }
        Activity a = TinyPlatform.activity();
        if (a == null || closed) {
            return;
        }
        Intent back = new Intent(a, TinyActivity.class).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        TinyPlatform.ui().post(() -> {
            try {
                a.startActivity(back);
            } catch (Exception e) {
                Log.w(TinyPlatform.TAG, "bringing the app back", e);
            }
        });
    }
}
