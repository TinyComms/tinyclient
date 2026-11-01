//! Discord, Slack and Teams sign-in: a `WKWebView` in a sheet over the GPUI window.
//!
//! Each network has its own persistent website data store (iOS 17+; older systems share the
//! default one), so a second sign-in resumes the first. Every decision (which page, which
//! navigations, which probes) stays in Rust (`login::web::sign_in`); this only shows the page,
//! answers navigations through `WebPage::navigate`, runs scripts and reads cookies.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::CGRect;
use objc2_foundation::{
    NSArray, NSError, NSHTTPCookie, NSJSONSerialization, NSJSONWritingOptions, NSString, NSURLErrorDomain, NSURLRequest,
    NSUUID, NSURL,
};
use objc2_ui_kit::{
    UIAdaptivePresentationControllerDelegate, UIApplication, UIBarButtonItem, UIBarButtonSystemItem,
    UINavigationController, UIPresentationController, UIViewController, UIWindowScene,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKUIDelegate, WKUserScript,
    WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use protocol::web_sign_in::{Navigation, PageState, WebPage, WebPageHandle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a script or a cookie read may take before it counts as "no answer".
const ANSWER: Duration = Duration::from_secs(5);

/// What the page's delegate learns, read by the handle from chatd's thread.
#[derive(Default)]
struct Inner {
    url: String,
    closed: bool,
    load_failed: bool,
}

type Navigate = Arc<dyn Fn(&str) -> Navigation + Send + Sync>;

struct Ivars {
    id: u64,
    navigate: Navigate,
    inner: Arc<Mutex<Inner>>,
}

/// The views of a showing page, kept on the main thread.
struct Views {
    web: Retained<WKWebView>,
    sheet: Retained<UINavigationController>,
    _delegate: Retained<PageDelegate>,
}

thread_local! {
    /// Main thread only.
    static PAGES: RefCell<HashMap<u64, Views>> = RefCell::new(HashMap::new());
}

static NEXT: AtomicU64 = AtomicU64::new(1);

fn text(url: Option<Retained<NSURL>>) -> String {
    url.and_then(|url| url.absoluteString()).map(|text| text.to_string()).unwrap_or_default()
}

/// The URL error codes that mean "no network": offline, DNS, unreachable, timeout, TLS.
fn network_failure(error: &NSError) -> bool {
    // SAFETY: Foundation's constant.
    let domain = unsafe { NSURLErrorDomain };
    error.domain().isEqualToString(domain)
        && matches!(error.code(), -1001 | -1003 | -1004 | -1005 | -1006 | -1009 | -1018 | -1020 | -2000 | -1206..=-1200)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Main-thread only, as every protocol it
    // adopts is.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TinyWebPageDelegate"]
    #[ivars = Ivars]
    struct PageDelegate;

    unsafe impl NSObjectProtocol for PageDelegate {}

    unsafe impl WKNavigationDelegate for PageDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            web: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: reading the action's request and frame.
            let (url, main) = unsafe {
                (text(action.request().URL()), action.targetFrame().map(|frame| frame.isMainFrame()).unwrap_or(true))
            };
            if !main || url.is_empty() || url == "about:blank" {
                handler.call((WKNavigationActionPolicy::Allow,));
                return;
            }
            match (self.ivars().navigate)(&url) {
                Navigation::Allow => handler.call((WKNavigationActionPolicy::Allow,)),
                Navigation::Cancel => handler.call((WKNavigationActionPolicy::Cancel,)),
                Navigation::Load(other) => {
                    handler.call((WKNavigationActionPolicy::Cancel,));
                    load_in(web, &other);
                }
            }
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, web: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Ok(mut inner) = self.ivars().inner.lock() {
                inner.load_failed = false;
                // SAFETY: reading the web view's URL on the main thread.
                inner.url = text(unsafe { web.URL() });
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, web: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Ok(mut inner) = self.ivars().inner.lock() {
                // SAFETY: reading the web view's URL on the main thread.
                inner.url = text(unsafe { web.URL() });
            }
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed_late(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }
    }

    unsafe impl WKUIDelegate for PageDelegate {
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(
            &self,
            web: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            // SAFETY: reading the action's request.
            let url = text(unsafe { action.request().URL() });
            if !url.is_empty() {
                match (self.ivars().navigate)(&url) {
                    Navigation::Allow => load_in(web, &url),
                    Navigation::Load(other) => load_in(web, &other),
                    Navigation::Cancel => {}
                }
            }
            None
        }
    }

    unsafe impl UIAdaptivePresentationControllerDelegate for PageDelegate {
        #[unsafe(method(presentationControllerDidDismiss:))]
        fn swiped_away(&self, _controller: &UIPresentationController) {
            self.mark_closed();
            let id = self.ivars().id;
            PAGES.with(|pages| pages.borrow_mut().remove(&id));
        }
    }

    impl PageDelegate {
        #[unsafe(method(closeTapped:))]
        fn close_tapped(&self, _sender: Option<&AnyObject>) {
            self.mark_closed();
            dismiss(self.ivars().id);
        }
    }
);

impl PageDelegate {
    fn new(mtm: MainThreadMarker, ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }

    fn failed(&self, error: &NSError) {
        if network_failure(error) {
            log::info!("tinyclient: sign-in page failed to load: {}", error.localizedDescription());
            if let Ok(mut inner) = self.ivars().inner.lock() {
                inner.load_failed = true;
            }
        }
    }

    fn mark_closed(&self) {
        if let Ok(mut inner) = self.ivars().inner.lock() {
            inner.closed = true;
        }
    }
}

fn load_in(web: &WKWebView, url: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
        // SAFETY: loading a request on the main thread.
        unsafe { web.loadRequest(&NSURLRequest::requestWithURL(&url)) };
    }
}

/// Takes page `id` away (main thread).
fn dismiss(id: u64) {
    if let Some(views) = PAGES.with(|pages| pages.borrow_mut().remove(&id)) {
        // SAFETY: dismissing the sheet this module presented, on the main thread.
        unsafe { views.sheet.dismissViewControllerAnimated_completion(true, None) };
    }
}

/// The view controller on top of the key window, to present over.
pub(super) fn top_controller(mtm: MainThreadMarker) -> Option<Retained<UIViewController>> {
    let app = UIApplication::sharedApplication(mtm);
    // SAFETY: walking UIKit's scene and window lists on the main thread.
    unsafe {
        let mut root = None;
        for scene in app.connectedScenes().iter() {
            let Ok(scene) = scene.downcast::<UIWindowScene>() else {
                continue;
            };
            let windows = scene.windows();
            let window = windows.iter().find(|window| window.isKeyWindow()).or_else(|| windows.firstObject());
            if let Some(window) = window {
                root = window.rootViewController();
                if root.is_some() {
                    break;
                }
            }
        }
        let mut top = root?;
        while let Some(next) = top.presentedViewController() {
            if next.isBeingDismissed() {
                break;
            }
            top = next;
        }
        Some(top)
    }
}

/// A store of its own per network, so its cookies and storage persist between sign-ins.
fn data_store(mtm: MainThreadMarker, network: &str) -> Retained<WKWebsiteDataStore> {
    if objc2::available!(ios = 17.0) {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(format!("tinyclient web sign-in {network}").as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let uuid = NSUUID::from_bytes(bytes);
        // SAFETY: iOS 17's persistent store by identifier, on the main thread.
        unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&uuid, mtm) }
    } else {
        // SAFETY: the default persistent store, on the main thread.
        unsafe { WKWebsiteDataStore::defaultDataStore(mtm) }
    }
}

/// Shows `page` and returns its handle at once, the page loading.
pub(super) fn open(page: WebPage) -> Result<IosPage, String> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let inner = Arc::new(Mutex::new(Inner { url: page.url.clone(), ..Inner::default() }));
    let shared = inner.clone();
    super::on_main(move |mtm| -> Result<(), String> {
        let top = top_controller(mtm).ok_or_else(|| "The sign-in page has no window to open over.".to_owned())?;
        // SAFETY: WebKit and UIKit set up on the main thread, as their APIs say.
        unsafe {
            let configuration = WKWebViewConfiguration::new(mtm);
            configuration.setWebsiteDataStore(&data_store(mtm, &page.network));
            if let Some(script) = &page.init_script {
                let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                    WKUserScript::alloc(mtm),
                    &NSString::from_str(script),
                    WKUserScriptInjectionTime::AtDocumentStart,
                    false,
                );
                configuration.userContentController().addUserScript(&script);
            }
            let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), CGRect::default(), &configuration);
            web.setCustomUserAgent(Some(&NSString::from_str(&page.user_agent)));
            let delegate = PageDelegate::new(mtm, Ivars { id, navigate: page.navigate.clone(), inner: shared });
            web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            web.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));

            let controller = UIViewController::new(mtm);
            controller.setView(Some(&web));
            controller.setTitle(Some(&NSString::from_str(&page.title)));
            let close = UIBarButtonItem::initWithBarButtonSystemItem_target_action(
                UIBarButtonItem::alloc(mtm),
                UIBarButtonSystemItem::Close,
                Some(&delegate),
                Some(sel!(closeTapped:)),
            );
            controller.navigationItem().setLeftBarButtonItem(Some(&close));
            let sheet = UINavigationController::initWithRootViewController(UINavigationController::alloc(mtm), &controller);
            if let Some(presentation) = sheet.presentationController() {
                presentation.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            }
            load_in(&web, &page.url);
            top.presentViewController_animated_completion(&sheet, true, None);
            log::info!("tinyclient: {} sign-in page shown", page.network);
            PAGES.with(|pages| pages.borrow_mut().insert(id, Views { web, sheet, _delegate: delegate }));
        }
        Ok(())
    })?;
    Ok(IosPage { id, inner })
}

/// The handle chatd holds.
pub(super) struct IosPage {
    id: u64,
    inner: Arc<Mutex<Inner>>,
}

/// The page's web view, if it is still showing (main thread).
fn web_of(id: u64) -> Option<Retained<WKWebView>> {
    PAGES.with(|pages| pages.borrow().get(&id).map(|views| views.web.clone()))
}

/// JSON text of an Objective-C value WebKit returned (`nil` is `null`).
fn json(value: Option<&AnyObject>) -> Option<String> {
    let Some(value) = value else {
        return Some("null".to_owned());
    };
    // Wrapped in an array, so strings and numbers (JSON fragments) serialize on every iOS.
    let array = NSArray::from_slice(&[value]);
    // SAFETY: the object is checked as JSON-able first, so serialization cannot raise.
    unsafe {
        if !NSJSONSerialization::isValidJSONObject(&array) {
            return None;
        }
        let data = NSJSONSerialization::dataWithJSONObject_options_error(&array, NSJSONWritingOptions::empty()).ok()?;
        let text = String::from_utf8(data.to_vec()).ok()?;
        let inside = text.strip_prefix('[')?.strip_suffix(']')?;
        Some(inside.to_owned())
    }
}

impl WebPageHandle for IosPage {
    fn state(&mut self) -> PageState {
        let id = self.id;
        let url = super::on_main(move |_| web_of(id).map(|web| text(unsafe { web.URL() })));
        let mut inner = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(url) = url.filter(|url| !url.is_empty()) {
            inner.url = url;
        }
        PageState { url: inner.url.clone(), closed: inner.closed, load_failed: inner.load_failed }
    }

    fn evaluate(&mut self, script: &str) -> Option<String> {
        let (sender, answer) = mpsc::channel::<Option<String>>();
        let id = self.id;
        let script = script.to_owned();
        let shown = super::on_main(move |_| {
            let Some(web) = web_of(id) else {
                return false;
            };
            let block = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
                // SAFETY: WebKit's result and error, valid for the call.
                let answer = if error.is_null() { json(unsafe { value.as_ref() }) } else { None };
                let _ = sender.send(answer);
            });
            // SAFETY: running a script in the main frame, on the main thread.
            unsafe { web.evaluateJavaScript_completionHandler(&NSString::from_str(&script), Some(&block)) };
            true
        });
        if !shown {
            return None;
        }
        answer.recv_timeout(ANSWER).ok().flatten()
    }

    fn cookie(&mut self, domain: &str, name: &str) -> Option<String> {
        let (sender, answer) = mpsc::channel::<Option<String>>();
        let id = self.id;
        let domain = domain.trim_start_matches('.').to_ascii_lowercase();
        let name = name.to_owned();
        let shown = super::on_main(move |_| {
            let Some(web) = web_of(id) else {
                return false;
            };
            let block = block2::RcBlock::new(move |cookies: std::ptr::NonNull<NSArray<NSHTTPCookie>>| {
                // SAFETY: WebKit's cookie list, valid for the call.
                let cookies = unsafe { cookies.as_ref() };
                let found = cookies.iter().find_map(|cookie| {
                    let host = cookie.domain().to_string().trim_start_matches('.').to_ascii_lowercase();
                    let matches = host == domain || host.ends_with(&format!(".{domain}"));
                    (matches && cookie.name().to_string() == name).then(|| cookie.value().to_string())
                });
                let _ = sender.send(found);
            });
            // SAFETY: the page's own store, read on the main thread.
            unsafe { web.configuration().websiteDataStore().httpCookieStore().getAllCookies(&block) };
            true
        });
        if !shown {
            return None;
        }
        answer.recv_timeout(ANSWER).ok().flatten()
    }

    fn load(&mut self, url: &str) {
        let id = self.id;
        let url = url.to_owned();
        super::on_main(move |_| {
            if let Some(web) = web_of(id) {
                load_in(&web, &url);
            }
        });
    }

    fn close(&mut self) {
        let id = self.id;
        super::on_main(move |_| dismiss(id));
    }
}
