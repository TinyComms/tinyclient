package dev.tinyclient.app;

import static org.junit.Assert.*;
import static org.robolectric.Shadows.shadowOf;

import android.app.Application;
import android.app.Service;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ResolveInfo;
import android.content.pm.ServiceInfo;
import android.net.Uri;
import android.os.Bundle;
import android.os.Looper;
import androidx.browser.customtabs.CustomTabsService;
import androidx.browser.customtabs.CustomTabsSessionToken;
import java.lang.reflect.Field;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.LooperMode;

/** Exercises the real AndroidX session Binder against a local browser service, without native UI. */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 36)
@LooperMode(LooperMode.Mode.PAUSED)
public class AuthSessionTest {
    private Application app;
    private Browser browser;
    private AuthSession session;

    @Before public void setUp() throws Exception {
        app = RuntimeEnvironment.getApplication();
        Field context = TinyPlatform.class.getDeclaredField("app");
        context.setAccessible(true);
        context.set(null, app);
        ResolveInfo resolve = new ResolveInfo();
        resolve.serviceInfo = new ServiceInfo();
        resolve.serviceInfo.packageName = "com.android.chrome";
        resolve.serviceInfo.name = Browser.class.getName();
        shadowOf(app.getPackageManager()).addResolveInfoForIntent(
                new Intent("android.support.customtabs.action.CustomTabsService"), resolve);
        browser = Robolectric.buildService(Browser.class).create().get();
        shadowOf(app).setComponentNameAndServiceForBindService(
                new ComponentName("com.android.chrome", Browser.class.getName()),
                browser.onBind(new Intent()));
    }

    @After public void tearDown() {
        if (session != null) session.close();
        shadowOf(Looper.getMainLooper()).idle();
    }

    private Intent open() {
        session = AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback");
        assertNotNull(session);
        awaitRegistration();
        return shadowOf(app).getNextStartedActivity();
    }

    private void awaitRegistration() {
        try {
            Field field = AuthSession.class.getDeclaredField("ready");
            field.setAccessible(true);
            CountDownLatch ready = (CountDownLatch) field.get(session);
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5);
            do {
                shadowOf(Looper.getMainLooper()).idle();
                if (ready.getCount() == 0) return;
                Thread.sleep(2);
            } while (System.nanoTime() < deadline);
            fail("Browser registration did not complete");
        } catch (Exception error) {
            throw new AssertionError(error);
        }
    }

    @Test public void browserCanAssociateKeepAliveWithItsRegisteredClientSession() throws Exception {
        Intent launched = open();
        assertNotNull(launched);
        CustomTabsSessionToken token = CustomTabsSessionToken.getSessionTokenFromIntent(launched);
        assertNotNull("A null SESSION cannot authorize Chrome's incoming keepalive binding", token);
        assertEquals(browser.registered, token);
        Intent keepAlive = launched.getParcelableExtra("android.support.customtabs.extra.KEEP_ALIVE", Intent.class);
        assertNotNull("The foreground browser must bind back to the loopback listener's process", keepAlive);
        assertEquals(app.getPackageName(), keepAlive.getComponent().getPackageName());
        Service service = (Service) Class.forName(keepAlive.getComponent().getClassName()).getConstructor().newInstance();
        assertNotNull(service.onBind(keepAlive));
        session.close();
        assertNull("An ended auth session must not accept another keepalive binding", service.onBind(keepAlive));
    }

    @Test public void pageIsNotLaunchedBeforeBrowserRegistersSession() {
        session = AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback");
        assertNotNull(session);
        assertNull("No Custom Tab without a registered session", shadowOf(app).getNextStartedActivity());
        awaitRegistration();
        assertNotNull(shadowOf(app).getNextStartedActivity());
    }

    @Test public void rejectingSessionDoesNotOpenAnUnprotectedBrowser() {
        browser.acceptSession = false;
        session = AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback");
        assertNotNull(session);
        awaitRegistration();
        assertTrue(session.isClosed());
        assertNull(shadowOf(app).getNextStartedActivity());
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
    }

    @Test public void cancellationReleasesBrowserConnectionAndDeadline() {
        open();
        AuthSession.onActivityPaused();
        AuthSession.onActivityResumed();
        assertTrue(session.isClosed());
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(6));
        assertNull(shadowOf(app).getNextStartedActivity());
    }

    @Test public void finishingIsIdempotentAndReleasesBrowserConnection() {
        open();
        session.close();
        session.close();
        shadowOf(Looper.getMainLooper()).idle();
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
    }

    @Test public void abandonedAuthHasABoundedLifetime() {
        open();
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(6));
        assertTrue(session.isClosed());
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
    }

    @Test public void closingBeforeConnectionPreventsLateLaunch() {
        session = AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback");
        session.close();
        shadowOf(Looper.getMainLooper()).idle();
        assertNull(shadowOf(app).getNextStartedActivity());
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
    }

    @Test public void closingDuringBrowserRegistrationPreventsLateLaunch() {
        session = AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback");
        browser.onRegistration = () -> session.close();
        awaitRegistration();
        assertNull(shadowOf(app).getNextStartedActivity());
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
    }

    @Test public void slowBrowserRegistrationDoesNotBlockUiTimeoutOrCleanup() throws Exception {
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        AtomicBoolean registeredOnUi = new AtomicBoolean();
        browser.onRegistration = () -> {
            registeredOnUi.set(Looper.myLooper() == Looper.getMainLooper());
            entered.countDown();
            try { release.await(2, TimeUnit.SECONDS); }
            catch (InterruptedException error) { Thread.currentThread().interrupt(); }
        };
        session = AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback");
        try {
            shadowOf(Looper.getMainLooper()).idle();
            assertTrue(entered.await(2, TimeUnit.SECONDS));
            assertFalse("Synchronous browser IPC must not run on Tiny's UI thread", registeredOnUi.get());
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(4));
            assertTrue("Registration deadline must execute while the browser is blocked", session.isClosed());
            assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
            assertNull(shadowOf(app).getNextStartedActivity());
        } finally {
            release.countDown();
        }
    }

    @Test public void rustWorkerReturnsOnlyAfterBrowserHasRegisteredAndLaunched() throws Exception {
        CompletableFuture<AuthSession> pending = CompletableFuture.supplyAsync(() ->
                AuthSession.open(7, "https://login.example/authorize", "http://127.0.0.1:8126/callback"));
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5);
        while (!pending.isDone() && System.nanoTime() < deadline) {
            shadowOf(Looper.getMainLooper()).idle();
            Thread.sleep(5);
        }
        session = pending.get(1, TimeUnit.SECONDS);
        assertNotNull(session);
        assertNotNull(browser.registered);
        assertNotNull(shadowOf(app).getNextStartedActivity());
    }

    @Test public void browserDisconnectReleasesSession() {
        open();
        assertEquals(1, shadowOf(app).getBoundServiceConnections().size());
        shadowOf(app).getBoundServiceConnections().get(0).onServiceDisconnected(
                new ComponentName("com.android.chrome", Browser.class.getName()));
        assertTrue(session.isClosed());
        assertEquals(0, shadowOf(app).getBoundServiceConnections().size());
    }

    public static class Browser extends CustomTabsService {
        CustomTabsSessionToken registered;
        boolean acceptSession = true;
        Runnable onRegistration;
        @Override protected boolean warmup(long flags) { return true; }
        @Override protected boolean newSession(CustomTabsSessionToken token) {
            registered = token;
            if (onRegistration != null) onRegistration.run();
            return acceptSession;
        }
        @Override protected boolean mayLaunchUrl(CustomTabsSessionToken t, Uri u, Bundle b, List<Bundle> l) { return false; }
        @Override protected Bundle extraCommand(String s, Bundle b) { return null; }
        @Override protected boolean updateVisuals(CustomTabsSessionToken t, Bundle b) { return false; }
        @Override protected boolean requestPostMessageChannel(CustomTabsSessionToken t, Uri u) { return false; }
        @Override protected int postMessage(CustomTabsSessionToken t, String s, Bundle b) { return -1; }
        @Override protected boolean validateRelationship(CustomTabsSessionToken t, int r, Uri u, Bundle b) { return false; }
        @Override protected boolean receiveFile(CustomTabsSessionToken t, Uri u, int p, Bundle b) { return false; }
    }
}
