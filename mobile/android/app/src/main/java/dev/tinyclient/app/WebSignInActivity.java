package dev.tinyclient.app;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.graphics.Insets;
import android.os.Bundle;
import android.os.Message;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.webkit.CookieManager;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

/**
 * The in-app web view for a Discord, Slack or Teams sign-in ({@link WebSession}). Every
 * main-frame navigation and new window asks Rust first ({@code nativeNavigate}); the page's
 * cookies and storage persist in the app's WebView profile, so a second sign-in resumes.
 */
public class WebSignInActivity extends Activity {
    static final String SESSION = "session";

    WebView web;
    private WebSession session;

    @SuppressLint("SetJavaScriptEnabled")
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        long id = getIntent().getLongExtra(SESSION, 0);
        session = WebSession.get(id);
        if (session == null) {
            finish();
            return;
        }

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setBackgroundColor(Color.rgb(24, 24, 27));
        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        int pad = dp(8);
        bar.setPadding(pad, pad, pad, pad);
        Button cancel = new Button(this, null, android.R.attr.borderlessButtonStyle);
        cancel.setText("Cancel");
        cancel.setTextColor(Color.rgb(228, 228, 231));
        cancel.setOnClickListener(v -> userClosed());
        TextView title = new TextView(this);
        title.setText(session.title);
        title.setTextColor(Color.WHITE);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 17);
        title.setSingleLine(true);
        title.setPadding(pad, 0, pad, 0);
        bar.addView(cancel, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        bar.addView(title, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        root.addView(bar, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        web = new WebView(this);
        WebSettings settings = web.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setDatabaseEnabled(true);
        settings.setSupportMultipleWindows(true);
        settings.setJavaScriptCanOpenWindowsAutomatically(true);
        settings.setLoadWithOverviewMode(true);
        settings.setUseWideViewPort(true);
        if (session.userAgent != null && !session.userAgent.isEmpty()) {
            settings.setUserAgentString(session.userAgent);
        }
        CookieManager cookies = CookieManager.getInstance();
        cookies.setAcceptCookie(true);
        cookies.setAcceptThirdPartyCookies(web, true);
        web.setWebViewClient(new Client());
        web.setWebChromeClient(new Chrome());
        root.addView(web, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));

        root.setOnApplyWindowInsetsListener((view, insets) -> {
            Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.ime() | WindowInsets.Type.displayCutout());
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            return WindowInsets.CONSUMED;
        });
        setContentView(root);

        session.attached(this);
        String first = decide(session.url);
        if (first != null) {
            web.loadUrl(first);
        }
        Log.i(TinyPlatform.TAG, "the " + session.network + " sign-in page opened: " + session.url);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    /** Asks Rust about {@code url}: the URL to load (itself, or another), or null to drop it. */
    private String decide(String url) {
        String answer;
        try {
            answer = TinyPlatform.nativeNavigate(session.id, url);
        } catch (Throwable t) {
            Log.w(TinyPlatform.TAG, "navigation decision", t);
            answer = "A";
        }
        if (answer == null || answer.startsWith("C")) {
            Log.i(TinyPlatform.TAG, "sign-in navigation cancelled: " + url);
            return null;
        }
        if (answer.startsWith("L")) {
            return answer.substring(1);
        }
        return url;
    }

    private void userClosed() {
        session.closed = true;
        finish();
    }

    @Override
    public void onBackPressed() {
        if (web != null && web.canGoBack()) {
            web.goBack();
            return;
        }
        userClosed();
    }

    @Override
    protected void onDestroy() {
        if (session != null && !session.closedByRust && isFinishing()) {
            session.closed = true;
        }
        if (session != null && session.activity == this) {
            session.activity = null;
        }
        if (web != null) {
            web.destroy();
            web = null;
        }
        super.onDestroy();
    }

    private final class Client extends WebViewClient {
        @Override
        public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
            if (!request.isForMainFrame()) {
                return false;
            }
            String url = request.getUrl().toString();
            String go = decide(url);
            if (go == null) {
                return true;
            }
            if (!go.equals(url)) {
                view.loadUrl(go);
                return true;
            }
            return false;
        }

        @Override
        public void onPageStarted(WebView view, String url, Bitmap favicon) {
            session.currentUrl = url;
            session.loadFailed = false;
            if (session.initScript != null && !session.initScript.isEmpty()) {
                view.evaluateJavascript(session.initScript, null);
            }
        }

        @Override
        public void onPageFinished(WebView view, String url) {
            session.currentUrl = url;
        }

        @Override
        public void onReceivedError(WebView view, WebResourceRequest request, WebResourceError error) {
            if (!request.isForMainFrame()) {
                return;
            }
            switch (error.getErrorCode()) {
                case ERROR_HOST_LOOKUP:
                case ERROR_CONNECT:
                case ERROR_TIMEOUT:
                case ERROR_FAILED_SSL_HANDSHAKE:
                case ERROR_IO:
                case ERROR_PROXY_AUTHENTICATION:
                    Log.w(TinyPlatform.TAG, "sign-in page failed to load: " + error.getDescription());
                    session.loadFailed = true;
                    break;
                default:
                    break;
            }
        }
    }

    private final class Chrome extends WebChromeClient {
        @Override
        public boolean onCreateWindow(WebView view, boolean isDialog, boolean isUserGesture, Message resultMsg) {
            // A new window: catch its first URL in a throwaway view and decide it here.
            WebView catcher = new WebView(WebSignInActivity.this);
            catcher.setWebViewClient(new WebViewClient() {
                @Override
                public boolean shouldOverrideUrlLoading(WebView v, WebResourceRequest request) {
                    take(request.getUrl().toString());
                    return true;
                }

                @Override
                public void onPageStarted(WebView v, String url, Bitmap favicon) {
                    take(url);
                }

                private boolean taken;

                private void take(String url) {
                    if (taken) {
                        return;
                    }
                    taken = true;
                    catcher.stopLoading();
                    String go = decide(url);
                    if (go != null && web != null) {
                        web.loadUrl(go);
                    }
                    catcher.post(catcher::destroy);
                }
            });
            WebView.WebViewTransport transport = (WebView.WebViewTransport) resultMsg.obj;
            transport.setWebView(catcher);
            resultMsg.sendToTarget();
            return true;
        }
    }
}
