package dev.tinyclient.app;

import android.app.Service;
import android.content.Intent;
import android.os.Binder;
import android.os.IBinder;
import android.util.Log;

/**
 * Chromium's Custom Tabs KEEP_ALIVE target. The foreground browser binds to this empty
 * service only while its auth tab is visible, preserving Tiny's loopback callback listener.
 * It exposes no app data or operations and is never started by Tiny.
 */
public final class AuthKeepAliveService extends Service {
    private final Binder binder = new Binder();

    @Override public IBinder onBind(Intent intent) {
        if (!AuthSession.needsKeepAlive()) return null;
        Log.i(TinyPlatform.TAG, "Tiny sign-in keepalive bound");
        return binder;
    }

    @Override public boolean onUnbind(Intent intent) {
        Log.i(TinyPlatform.TAG, "Tiny sign-in keepalive unbound");
        return false;
    }

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        // Export is needed for the browser's binding, not for running a background service.
        stopSelf(startId);
        return START_NOT_STICKY;
    }
}
