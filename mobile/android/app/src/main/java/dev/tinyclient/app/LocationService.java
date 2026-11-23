package dev.tinyclient.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;
import android.util.Log;

/**
 * Foreground service kept alive while the user is actively sharing their live location
 * in one or more chats (#452). Shows a persistent ongoing notification so the user always
 * sees that live location sharing is active and can tap to return and stop it.
 */
public class LocationService extends Service {
    private static final String CHANNEL = "live_location";
    private static final int NOTIFICATION = 7105;
    private static final String EXTRA_SUMMARY = "summary";

    public static void start(Context c, String summary) {
        if (c == null) {
            return;
        }
        Intent intent = new Intent(c, LocationService.class).putExtra(EXTRA_SUMMARY, summary);
        try {
            c.startForegroundService(intent);
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "live location foreground service did not start", e);
        }
    }

    public static void stop(Context c) {
        if (c != null) {
            c.stopService(new Intent(c, LocationService.class));
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String text = intent != null ? intent.getStringExtra(EXTRA_SUMMARY) : null;
        if (text == null || text.isEmpty()) {
            text = "Sharing live location";
        }
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel channel = new NotificationChannel(
                    CHANNEL,
                    "Live Location",
                    NotificationManager.IMPORTANCE_LOW
            );
            channel.setDescription("Shown while you are actively sharing your live location");
            nm.createNotificationChannel(channel);
        }
        Intent open = new Intent(this, TinyActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent tap = PendingIntent.getActivity(
                this,
                0,
                open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT
        );
        Notification notification = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.ic_menu_mylocation)
                .setContentTitle("Tiny · Live location")
                .setContentText(text)
                .setContentIntent(tap)
                .setOngoing(true)
                .setCategory(Notification.CATEGORY_SERVICE)
                .build();
        try {
            startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION);
        } catch (Exception e) {
            Log.w(TinyPlatform.TAG, "location foreground service refused", e);
        }
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
