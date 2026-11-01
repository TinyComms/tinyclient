package dev.tinyclient.app;

import android.content.Intent;
import android.os.Bundle;

import dev.gpui.mobile.GpuiInputActivity;

/**
 * The app's one window: gpui_ce_mobile's NativeActivity with the results only an activity
 * receives (runtime permissions, the screen-capture consent) and the moments the Tiny sign-in
 * needs (the Custom Tab closed by the user), handed to {@link TinyPlatform}.
 */
public class TinyActivity extends GpuiInputActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        TinyPlatform.attach(this);
        super.onCreate(savedInstanceState);
        selfTest(getIntent());
    }

    private void selfTest(Intent intent) {
        if (intent == null || (getApplicationInfo().flags & android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) == 0) {
            return;
        }
        String what = intent.getStringExtra("tiny.selftest");
        if (what != null) {
            TinyPlatform.nativeSelfTest(what);
        }
        // A debug launch for call tests without taps:
        //   am start -n dev.tinyclient.app/.TinyActivity --es tiny.join "<server>|<room>" \
        //       --ez tiny.camera true --ez tiny.speaker true
        String join = intent.getStringExtra("tiny.join");
        if (join != null) {
            try {
                TinyPlatform.nativeDebugJoin(join, intent.getBooleanExtra("tiny.camera", false), intent.getBooleanExtra("tiny.speaker", false));
            } catch (UnsatisfiedLinkError notDebugRust) {
                android.util.Log.w("tinyclient", "tiny.join needs a debug build of the Rust library");
            }
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        TinyPlatform.onPermissionsResult(requestCode, permissions, grantResults);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        TinyPlatform.onActivityResult(requestCode, resultCode, data);
    }

    @Override
    protected void onPause() {
        super.onPause();
        AuthSession.onActivityPaused();
    }

    @Override
    protected void onResume() {
        super.onResume();
        AuthSession.onActivityResumed();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        selfTest(intent);
    }

    @Override
    protected void onDestroy() {
        TinyPlatform.detach(this);
        super.onDestroy();
    }
}
