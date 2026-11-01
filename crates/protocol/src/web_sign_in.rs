//! Sign-in through a web page, shown by the platform instead of a helper process (phones).
//!
//! On a desktop chatd runs `tinyclient-login <network>`, a window with a web view, and reads one
//! line of JSON back. A phone cannot run a helper, so its mobile entry registers a [`WebSignIn`]
//! here, and chatd drives that instead. The platform only shows pages and answers questions about
//! them; what to open, what to read, when it is done and the session it makes are decided in Rust
//! (`login::web`), the same code the desktop helper runs.
//!
//! Two kinds of page:
//!
//! * [`WebSignIn::open_page`]: an in-app web view (WKWebView, android.webkit.WebView) whose
//!   storage and cookies chatd can read. Discord, Slack and Teams. Its cookies and storage persist
//!   per `WebPage::network` between sign-ins, as the desktop helper keeps its browser.
//! * [`WebSignIn::open_auth`]: the system's authentication session (iOS
//!   `ASWebAuthenticationSession`, Android Custom Tabs) for an OAuth/OIDC authorize page that
//!   comes back to a loopback redirect. A Tiny server's sign-in, which needs passkeys, and passkeys
//!   do not work in a plain WKWebView or WebView.
//!
//! Threading: chatd calls these from its own request threads, never the platform's UI thread, and
//! blocks while it waits. The platform hops to its UI thread itself. `WebPage::navigate` and
//! `AuthRequest::redirected` are the exceptions: the platform calls them from whatever thread
//! sees the navigation (usually its UI thread), and they return at once.
//!
//! See `docs/mobile-platform.md`.

use std::sync::{Arc, RwLock};

/// What the platform shows web pages with.
pub trait WebSignIn: Send + Sync {
    /// Shows `page` in an in-app web view and returns at once, the page loading. `Err` with
    /// words when no page can be shown at all.
    fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String>;

    /// Shows `auth.url` in the system's authentication session and returns at once. The redirect
    /// goes to `auth.callback`, a loopback `http://127.0.0.1:<port>/callback` that chatd is
    /// listening on; the platform need not intercept it. If it does see the navigation (a session
    /// that cannot reach loopback, or one that stops at the callback), it passes the whole URL to
    /// `auth.redirected` and chatd delivers it to its listener itself.
    fn open_auth(&self, auth: AuthRequest) -> Result<Box<dyn AuthHandle>, String>;
}

/// One in-app sign-in page.
pub struct WebPage {
    /// `discord`, `slack` or `teams`. Also names the persistent web data store to use, so a
    /// second sign-in resumes the first.
    pub network: String,
    /// The window or sheet title.
    pub title: String,
    /// The first page.
    pub url: String,
    /// The User-Agent every request of the page sends. The session records it and chatd goes on
    /// using it, so the network sees one browser.
    pub user_agent: String,
    /// Run at document start in every frame when `Some`. No network uses one yet.
    pub init_script: Option<String>,
    /// Asked about every main-frame navigation and every new-window request, before it happens.
    /// Called on the platform's own thread; quick and never blocking.
    pub navigate: Arc<dyn Fn(&str) -> Navigation + Send + Sync>,
}

/// What to do with a navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Navigation {
    /// Let it happen. For a new-window request: load it in this page instead of a new window.
    Allow,
    /// Drop it.
    Cancel,
    /// Drop it and load this URL in the page instead.
    Load(String),
}

/// How the page is, now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    /// The page's current URL, empty before the first one commits.
    pub url: String,
    /// The user closed it (back, swipe down, the close button).
    pub closed: bool,
    /// The last load failed for want of a network: no connection, DNS, TLS, timeout.
    pub load_failed: bool,
}

/// A page that is showing. Every method may block for a moment while the platform asks its UI
/// thread; none may block for more than a few seconds.
pub trait WebPageHandle: Send {
    fn state(&mut self) -> PageState;
    /// Runs `script` in the page's main frame and returns its value as JSON text (`"abc"`,
    /// `null`, `{"a":1}`), as Android's `evaluateJavascript` gives it; iOS JSON-encodes what
    /// `evaluateJavaScript` returns. `None` when it threw, timed out or the page is gone.
    fn evaluate(&mut self, script: &str) -> Option<String>;
    /// The value of cookie `name` for `domain` (`slack.com`, matching its subdomains), from the
    /// page's own cookie store, HttpOnly ones included.
    fn cookie(&mut self, domain: &str, name: &str) -> Option<String>;
    /// Loads `url` in the page.
    fn load(&mut self, url: &str);
    /// Takes the page away. Called once, at the end, whatever the outcome.
    fn close(&mut self);
}

/// One authentication session.
pub struct AuthRequest {
    /// The authorize page.
    pub url: String,
    /// The redirect the page ends on, `http://127.0.0.1:<port>/callback`.
    pub callback: String,
    /// Takes the redirect URL, query and all, if the platform caught it. Any thread.
    pub redirected: Arc<dyn Fn(String) + Send + Sync>,
}

/// A showing authentication session.
pub trait AuthHandle: Send {
    /// The user dismissed it.
    fn closed(&mut self) -> bool;
    /// Takes it away (the sign-in finished or was cancelled). On Android, where a Custom Tab cannot
    /// be closed from outside, this brings the app's activity back to the front.
    fn close(&mut self);
}

static PROVIDER: RwLock<Option<Arc<dyn WebSignIn>>> = RwLock::new(None);

/// Registers the platform's web pages. A later registration replaces an earlier one.
pub fn register(provider: Arc<dyn WebSignIn>) {
    *PROVIDER.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(provider);
}

/// Removes the registration: chatd goes back to the helper process. For tests.
pub fn unregister() {
    *PROVIDER.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// The registered platform, if any.
pub fn provider() -> Option<Arc<dyn WebSignIn>> {
    PROVIDER.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}
