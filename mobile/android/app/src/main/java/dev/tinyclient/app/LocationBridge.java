package dev.tinyclient.app;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationManager;

/**
 * JNI bridge for Android device location fixes and the foreground live-location notification
 * service ({@link LocationService}) (#452). Never guesses location from IP.
 */
public final class LocationBridge {
    private LocationBridge() {}

    /**
     * Returns {@code "lat,lon,accuracy_m,heading_deg"} from the system {@link LocationManager},
     * or {@code "err:<reason>"} if location permission is missing or no fix is available.
     */
    public static String currentLocation() {
        Context c = TinyPlatform.context();
        if (c == null) {
            return "err:The app is not active.";
        }
        boolean fine = c.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION)
                == PackageManager.PERMISSION_GRANTED;
        boolean coarse = c.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION)
                == PackageManager.PERMISSION_GRANTED;
        if (!fine && !coarse) {
            return "err:Location permission has not been granted in Android Settings.";
        }
        LocationManager lm = c.getSystemService(LocationManager.class);
        if (lm == null) {
            return "err:Location services are unavailable on this device.";
        }
        Location best = null;
        for (String provider : new String[] {
                LocationManager.FUSED_PROVIDER,
                LocationManager.GPS_PROVIDER,
                LocationManager.NETWORK_PROVIDER
        }) {
            try {
                Location loc = lm.getLastKnownLocation(provider);
                if (loc != null && (best == null || loc.getTime() > best.getTime())) {
                    best = loc;
                }
            } catch (SecurityException | IllegalArgumentException ignored) {
            }
        }
        if (best == null) {
            return "err:No GPS fix is available yet. Move outdoors or enter coordinates.";
        }
        int accuracy = best.hasAccuracy() ? Math.max(0, Math.round(best.getAccuracy())) : 0;
        int heading = best.hasBearing() ? Math.round(best.getBearing()) : 0;
        return best.getLatitude() + "," + best.getLongitude() + "," + accuracy + "," + heading;
    }

    public static String startLiveTracking(String summary) {
        Context c = TinyPlatform.context();
        if (c == null) {
            return "The app is not active.";
        }
        LocationService.start(c, summary);
        return null;
    }

    public static void stopLiveTracking() {
        Context c = TinyPlatform.context();
        LocationService.stop(c);
    }
}
