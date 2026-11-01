package dev.tinyclient.app;

import android.content.Intent;
import android.util.Log;
import android.webkit.CookieManager;

import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * One Discord, Slack or Teams sign-in page as Rust drives it (protocol::web_sign_in's
 * WebPageHandle): shown in {@link WebSignInActivity}, polled from a chatd thread.
 */
public final class WebSession {
    private static final Map<Long, WebSession> open = new HashMap<>();

    final long id;
    final String network;
    final String title;
    final String url;
    final String userAgent;
    final String initScript;

    volatile WebSignInActivity activity;
    volatile String currentUrl;
    volatile boolean closed;
    volatile boolean loadFailed;
    volatile boolean closedByRust;
    private final CountDownLatch shown = new CountDownLatch(1);

    private WebSession(long id, String network, String title, String url, String userAgent, String initScript) {
        this.id = id;
        this.network = network;
        this.title = title;
        this.url = url;
        this.userAgent = userAgent;
        this.initScript = initScript;
        this.currentUrl = url;
    }

    static synchronized WebSession get(long id) {
        return open.get(id);
    }

    /** Opens the page for Rust's {@code id}; returns once the activity is up, or null. */
    public static WebSession open(long id, String network, String title, String url, String userAgent, String initScript) {
        android.content.Context c = TinyPlatform.context();
        if (c == null) {
            return null;
        }
        WebSession session = new WebSession(id, network, title, url, userAgent, initScript);
        synchronized (WebSession.class) {
            open.put(id, session);
        }
        Intent intent = new Intent(c, WebSignInActivity.class).putExtra(WebSignInActivity.SESSION, id);
        if (TinyPlatform.activity() == null) {
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        TinyPlatform.ui().post(() -> c.startActivity(intent));
        try {
            if (!session.shown.await(10, TimeUnit.SECONDS)) {
                Log.w(TinyPlatform.TAG, "the " + network + " sign-in page did not open");
                forget(id);
                return null;
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return null;
        }
        return session;
    }

    static synchronized void forget(long id) {
        open.remove(id);
    }

    void attached(WebSignInActivity a) {
        activity = a;
        shown.countDown();
    }

    /** "cf" flags then the URL: c closed by the user, f the last load failed for the network. */
    public String state() {
        return (closed ? "1" : "0") + (loadFailed ? "1" : "0") + (currentUrl == null ? "" : currentUrl);
    }

    /** Runs {@code script} in the page; the JSON text of its value, or null (error, ~5 s). */
    public String evaluate(String script) {
        WebSignInActivity a = activity;
        if (a == null || closed) {
            return null;
        }
        AtomicReference<String> answer = new AtomicReference<>();
        CountDownLatch done = new CountDownLatch(1);
        TinyPlatform.ui().post(() -> {
            if (a.web == null) {
                done.countDown();
                return;
            }
            a.web.evaluateJavascript(script, value -> {
                answer.set(value);
                done.countDown();
            });
        });
        try {
            if (!done.await(5, TimeUnit.SECONDS)) {
                return null;
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return null;
        }
        return answer.get();
    }

    /** The cookie {@code name} for {@code domain} (and its subdomains), HttpOnly included. */
    public String cookie(String domain, String name) {
        String found = TinyPlatform.onUi(3000, () -> {
            CookieManager cm = CookieManager.getInstance();
            for (String host : new String[] {domain, "www." + domain, "app." + domain}) {
                String all = cm.getCookie("https://" + host + "/");
                if (all == null) {
                    continue;
                }
                for (String pair : all.split(";")) {
                    String p = pair.trim();
                    int eq = p.indexOf('=');
                    if (eq > 0 && p.substring(0, eq).equals(name)) {
                        return p.substring(eq + 1);
                    }
                }
            }
            return null;
        });
        return found;
    }

    public void load(String other) {
        WebSignInActivity a = activity;
        if (a != null) {
            TinyPlatform.ui().post(() -> {
                if (a.web != null) {
                    a.web.loadUrl(other);
                }
            });
        }
    }

    /** Dismisses the page; called once at the end whatever happened. */
    public void close() {
        closedByRust = true;
        WebSignInActivity a = activity;
        if (a != null) {
            TinyPlatform.ui().post(a::finish);
        }
        TinyPlatform.ui().post(() -> CookieManager.getInstance().flush());
        forget(id);
    }
}
