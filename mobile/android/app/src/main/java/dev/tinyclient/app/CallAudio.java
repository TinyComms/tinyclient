package dev.tinyclient.app;

import android.content.Context;
import android.media.AudioAttributes;
import android.media.AudioDeviceInfo;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.os.Build;
import android.util.Log;

import java.util.List;

/**
 * A call's audio session: focus, {@code MODE_IN_COMMUNICATION}, the route (earpiece, speaker,
 * Bluetooth, wired) and the foreground service that keeps the call going in the background.
 * cpal opens AAudio itself once the session is up.
 */
public final class CallAudio {
    public static final int EARPIECE = 0;
    public static final int SPEAKER = 1;
    public static final int BLUETOOTH = 2;
    public static final int WIRED = 3;

    private static AudioFocusRequest focus;
    private static boolean active;
    private static int savedMode = AudioManager.MODE_NORMAL;
    private static boolean sco;

    private CallAudio() {}

    private static AudioManager manager() {
        Context c = TinyPlatform.context();
        return c == null ? null : (AudioManager) c.getSystemService(Context.AUDIO_SERVICE);
    }

    /** Starts the session with {@code route}. Returns null, or what went wrong in words. */
    public static synchronized String start(int route) {
        AudioManager am = manager();
        if (am == null) {
            return "The phone's audio isn't ready yet.";
        }
        if (!active) {
            AudioAttributes attributes = new AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build();
            focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                    .setAudioAttributes(attributes)
                    .setOnAudioFocusChangeListener(change -> Log.i(TinyPlatform.TAG, "call audio focus " + change))
                    .build();
            int got = am.requestAudioFocus(focus);
            if (got != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                Log.w(TinyPlatform.TAG, "call audio focus not granted: " + got);
            }
            savedMode = am.getMode();
            am.setMode(AudioManager.MODE_IN_COMMUNICATION);
            active = true;
            CallService.add(TinyPlatform.context(), CallService.MICROPHONE);
        }
        setRoute(route);
        Log.i(TinyPlatform.TAG, "call audio session started, route " + route);
        return null;
    }

    /** Ends the session: the route back, MODE_NORMAL, focus given back, the service stopped. */
    public static synchronized void stop() {
        AudioManager am = manager();
        if (am == null || !active) {
            return;
        }
        if (Build.VERSION.SDK_INT >= 31) {
            am.clearCommunicationDevice();
        } else {
            am.setSpeakerphoneOn(false);
        }
        stopSco(am);
        am.setMode(savedMode == AudioManager.MODE_IN_COMMUNICATION ? AudioManager.MODE_NORMAL : savedMode);
        if (focus != null) {
            am.abandonAudioFocusRequest(focus);
            focus = null;
        }
        active = false;
        CallService.clear(TinyPlatform.context());
        Log.i(TinyPlatform.TAG, "call audio session ended");
    }

    private static int typeOf(AudioDeviceInfo device) {
        switch (device.getType()) {
            case AudioDeviceInfo.TYPE_BUILTIN_EARPIECE:
                return EARPIECE;
            case AudioDeviceInfo.TYPE_BUILTIN_SPEAKER:
                return SPEAKER;
            case AudioDeviceInfo.TYPE_BLUETOOTH_SCO:
            case AudioDeviceInfo.TYPE_BLE_HEADSET:
            case AudioDeviceInfo.TYPE_HEARING_AID:
                return BLUETOOTH;
            case AudioDeviceInfo.TYPE_WIRED_HEADSET:
            case AudioDeviceInfo.TYPE_WIRED_HEADPHONES:
            case AudioDeviceInfo.TYPE_USB_HEADSET:
                return WIRED;
            default:
                return -1;
        }
    }

    /** The routes a call can play through now, as a bit set of 1 << EARPIECE etc. */
    public static int routes() {
        AudioManager am = manager();
        if (am == null) {
            return (1 << EARPIECE) | (1 << SPEAKER);
        }
        int mask = 0;
        if (Build.VERSION.SDK_INT >= 31) {
            for (AudioDeviceInfo device : am.getAvailableCommunicationDevices()) {
                int t = typeOf(device);
                if (t >= 0) {
                    mask |= 1 << t;
                }
            }
        } else {
            for (AudioDeviceInfo device : am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
                int t = typeOf(device);
                if (t >= 0) {
                    mask |= 1 << t;
                }
            }
        }
        // A phone without an earpiece (a tablet, some emulators) still offers the speaker.
        return mask == 0 ? (1 << SPEAKER) : mask | (1 << SPEAKER);
    }

    /** Moves the call's sound to {@code route}. Returns null, or what went wrong in words. */
    public static synchronized String setRoute(int route) {
        AudioManager am = manager();
        if (am == null) {
            return "The phone's audio isn't ready yet.";
        }
        if (Build.VERSION.SDK_INT >= 31) {
            List<AudioDeviceInfo> devices = am.getAvailableCommunicationDevices();
            AudioDeviceInfo pick = null;
            for (AudioDeviceInfo device : devices) {
                if (typeOf(device) == route) {
                    pick = device;
                    break;
                }
            }
            if (pick == null && route == EARPIECE) {
                am.clearCommunicationDevice();
                return null;
            }
            if (pick == null) {
                return "That audio route isn't connected.";
            }
            boolean ok = am.setCommunicationDevice(pick);
            Log.i(TinyPlatform.TAG, "call audio route " + route + " -> " + pick.getProductName() + (ok ? "" : " (refused)"));
            return ok ? null : "The phone didn't switch the audio route.";
        }
        switch (route) {
            case SPEAKER:
                stopSco(am);
                am.setSpeakerphoneOn(true);
                break;
            case BLUETOOTH:
                am.setSpeakerphoneOn(false);
                am.startBluetoothSco();
                am.setBluetoothScoOn(true);
                sco = true;
                break;
            default:
                stopSco(am);
                am.setSpeakerphoneOn(false);
                break;
        }
        return null;
    }

    private static void stopSco(AudioManager am) {
        if (sco) {
            am.setBluetoothScoOn(false);
            am.stopBluetoothSco();
            sco = false;
        }
    }
}
