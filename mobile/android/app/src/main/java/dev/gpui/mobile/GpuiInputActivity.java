package dev.gpui.mobile;

import android.app.NativeActivity;
import android.content.Context;
import android.graphics.Insets;
import android.os.Bundle;
import android.text.InputType;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * NativeActivity plus the Java half of gpui_ce_mobile's text input and insets.
 *
 * <p>NativeActivity alone gives the IME a key-event-only connection: no committed text, no
 * composition, and gpui_ce_mobile logs "IME requires GpuiInputActivity" and shows no keyboard.
 * gpui_ce_mobile calls the {@code gpui*} methods below from its render thread (names and
 * signatures are fixed by its jni.rs) and receives text and insets through the two natives.
 * Everything that touches a View is posted to the UI thread.
 *
 * <p>Written for tinyclient from that JNI contract; the event kinds match
 * gpui_ce_mobile's {@code android/text_input.rs}: 0 compose, 1 commit, 2 delete surrounding
 * (UTF-16), 3 delete surrounding (code points), 5 keyboard hidden by the user, 6 set selection.
 */
public class GpuiInputActivity extends NativeActivity {
    private static final int COMPOSE = 0;
    private static final int COMMIT = 1;
    private static final int DELETE = 2;
    private static final int DELETE_CODE_POINTS = 3;
    private static final int HIDDEN_BY_USER = 5;
    private static final int SELECT = 6;

    // NativeActivity dlopens the library itself, which does not bind `native` methods to this
    // class's loader; loading it here does (the second load returns the same handle).
    static {
        System.loadLibrary("tinyclient");
    }

    static native void nativeIme(long session, int kind, String text, int start, int end, long generation);

    native void nativeInsets(int safeTop, int safeRight, int safeBottom, int safeLeft,
                             int imeTop, int imeRight, int imeBottom, int imeLeft);

    private ImeProxy proxy;
    /** The GPUI input session the keyboard is shown for; 0 while it is hidden. */
    private volatile long session;
    private int inputType = InputType.TYPE_CLASS_TEXT;
    private long generation;
    private boolean imeVisible;

    // The focused GPUI field as the native side last reported it (gpuiUpdateInputSnapshot).
    private String snapshotText = "";
    private int snapshotSelStart;
    private int snapshotSelEnd;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        proxy = new ImeProxy(this);
        addContentView(proxy, new ViewGroup.LayoutParams(1, 1));
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
            Insets safe = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            Insets ime = insets.getInsets(WindowInsets.Type.ime());
            nativeInsets(safe.top, safe.right, safe.bottom, safe.left, ime.top, ime.right, ime.bottom, ime.left);
            boolean visible = insets.isVisible(WindowInsets.Type.ime());
            if (imeVisible && !visible && session != 0) {
                // Hidden with back or the keyboard's own button while the field keeps focus.
                send(HIDDEN_BY_USER, "", 0, 0);
            }
            imeVisible = visible;
            return view.onApplyWindowInsets(insets);
        });
    }

    private void send(int kind, String text, int start, int end) {
        long current = session;
        if (current != 0) {
            nativeIme(current, kind, text, start, end, ++generation);
        }
    }

    private InputMethodManager imm() {
        return (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
    }

    // ── called by gpui_ce_mobile (render thread) ─────────────────────────────

    public void gpuiShowKeyboard(int keyboardType, long newSession) {
        runOnUiThread(() -> {
            session = newSession;
            switch (keyboardType) {
                case 1: inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS; break;
                case 2: inputType = InputType.TYPE_CLASS_PHONE; break;
                case 3: inputType = InputType.TYPE_CLASS_NUMBER; break;
                case 4: inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI; break;
                case 5: inputType = InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL; break;
                default: inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
                        | InputType.TYPE_TEXT_FLAG_AUTO_CORRECT; break;
            }
            proxy.requestFocus();
            InputMethodManager imm = imm();
            imm.restartInput(proxy);
            imm.showSoftInput(proxy, 0);
        });
    }

    public void gpuiHideKeyboard(long oldSession) {
        runOnUiThread(() -> {
            session = 0;
            imm().hideSoftInputFromWindow(proxy.getWindowToken(), 0);
            proxy.clearFocus();
        });
    }

    public void gpuiResetComposition(long newSession) {
        runOnUiThread(() -> {
            session = newSession;
            imm().restartInput(proxy);
        });
    }

    public void gpuiUpdateInputSnapshot(long forSession, String text, int selStart, int selEnd,
                                        int markStart, int markEnd, long forGeneration) {
        runOnUiThread(() -> {
            if (forSession != session) {
                return;
            }
            snapshotText = text == null ? "" : text;
            snapshotSelStart = selStart;
            snapshotSelEnd = selEnd;
            imm().updateSelection(proxy, selStart, selEnd, markStart, markEnd);
        });
    }

    // ── the IME's view of the focused GPUI field ──────────────────────────────

    private final class ImeProxy extends View {
        ImeProxy(Context context) {
            super(context);
            setFocusable(true);
            setFocusableInTouchMode(true);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo info) {
            info.inputType = inputType;
            info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI;
            info.initialSelStart = snapshotSelStart;
            info.initialSelEnd = snapshotSelEnd;
            return new Connection(this);
        }
    }

    private final class Connection extends BaseInputConnection {
        private String composing = "";

        Connection(View view) {
            super(view, false);
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            composing = "";
            send(COMMIT, text.toString(), 0, 0);
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            composing = text.toString();
            int cursor = composing.length();
            send(COMPOSE, composing, cursor, cursor);
            return true;
        }

        @Override
        public boolean finishComposingText() {
            if (!composing.isEmpty()) {
                String text = composing;
                composing = "";
                send(COMMIT, text, 0, 0);
            }
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            send(DELETE, "", beforeLength, afterLength);
            return true;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
            send(DELETE_CODE_POINTS, "", beforeLength, afterLength);
            return true;
        }

        @Override
        public boolean setSelection(int start, int end) {
            send(SELECT, "", start, end);
            return true;
        }

        @Override
        public CharSequence getTextBeforeCursor(int n, int flags) {
            int end = Math.max(0, Math.min(snapshotSelStart, snapshotText.length()));
            return snapshotText.substring(Math.max(0, end - n), end);
        }

        @Override
        public CharSequence getTextAfterCursor(int n, int flags) {
            int start = Math.max(0, Math.min(snapshotSelEnd, snapshotText.length()));
            return snapshotText.substring(start, Math.min(snapshotText.length(), start + n));
        }

        @Override
        public boolean performEditorAction(int actionCode) {
            // A Done/Go/Send key: deliver it as the Enter key, as a hardware keyboard would.
            sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER));
            sendKeyEvent(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER));
            return true;
        }
    }
}
