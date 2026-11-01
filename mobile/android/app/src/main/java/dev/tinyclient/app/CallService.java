package dev.tinyclient.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * The foreground service a call runs under, with its notification: it keeps the microphone,
 * the camera and a screen share going while the app is in the background. Its types follow
 * what the call uses ({@link #add}, {@link #remove}); a share's type must be in place before
 * {@code getMediaProjection}, so {@link #add} waits until the service has said so.
 */
public class CallService extends Service {
    public static final int MICROPHONE = ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE;
    public static final int CAMERA = ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA;
    public static final int PROJECTION = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION;

    private static final String CHANNEL = "call";
    private static final int NOTIFICATION = 7102;
    private static final String TYPES = "types";

    private static int types;
    private static volatile CountDownLatch applied;

    /** Adds {@code type} to the running call's service, starting it, and waits until applied. */
    public static synchronized void add(Context c, int type) {
        if (c == null) {
            return;
        }
        types |= type;
        apply(c);
    }

    /** Drops {@code type}; the service stops with none left. */
    public static synchronized void remove(Context c, int type) {
        if (c == null) {
            return;
        }
        types &= ~type;
        if (types == 0) {
            c.stopService(new Intent(c, CallService.class));
            return;
        }
        apply(c);
    }

    /** The call ended: stop. */
    public static synchronized void clear(Context c) {
        types = 0;
        if (c != null) {
            c.stopService(new Intent(c, CallService.class));
        }
    }

    private static void apply(Context c) {
        CountDownLatch latch = new CountDownLatch(1);
        applied = latch;
        Intent intent = new Intent(c, CallService.class).putExtra(TYPES, types);
        try {
            c.startForegroundService(intent);
            latch.await(3, TimeUnit.SECONDS);
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "the call's foreground service did not start", e);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        int wanted = intent != null ? intent.getIntExtra(TYPES, MICROPHONE) : MICROPHONE;
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel channel = new NotificationChannel(CHANNEL, "Calls", NotificationManager.IMPORTANCE_LOW);
            channel.setDescription("Shown while you're in a call");
            nm.createNotificationChannel(channel);
        }
        Intent open = new Intent(this, TinyActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent tap = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        String text = (wanted & PROJECTION) != 0 ? "In a call, sharing your screen"
                : (wanted & CAMERA) != 0 ? "In a call, camera on" : "In a call";
        Notification notification = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.sym_call_outgoing)
                .setContentTitle("Tiny")
                .setContentText(text)
                .setContentIntent(tap)
                .setOngoing(true)
                .setCategory(Notification.CATEGORY_CALL)
                .build();
        try {
            startForeground(NOTIFICATION, notification, wanted);
            Log.i(TinyPlatform.TAG, "call service in the foreground, types " + wanted);
        } catch (Exception e) {
            // Android 14 refuses a type whose permission is missing or whose app is not visible.
            Log.w(TinyPlatform.TAG, "call service types " + wanted + " refused; microphone only", e);
            try {
                startForeground(NOTIFICATION, notification, MICROPHONE);
            } catch (Exception again) {
                Log.w(TinyPlatform.TAG, "call service not in the foreground", again);
            }
        }
        CountDownLatch latch = applied;
        if (latch != null) {
            latch.countDown();
        }
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onTimeout(int startId, int fgsType) {
        stopSelf();
    }
}
