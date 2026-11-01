//! Discord, Slack and Teams sign-in: a `WKWebView` in a sheet over the GPUI window.
//!
//! Each network has its own persistent website data store (iOS 17+; older systems share the
//! default one), so a second sign-in resumes the first. Every decision (which page, which
//! navigations, which probes) stays in Rust (`login::web::sign_in`); this only shows the page,
//! answers navigations through `WebPage::navigate`, runs scripts and reads cookies.

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::CGRect;
use objc2_foundation::{
    NSArray, NSError, NSHTTPCookie, NSInteger, NSJSONSerialization, NSJSONWritingOptions, NSString, NSURLErrorDomain,
    NSURLRequest, NSUUID, NSURL,
};
use objc2_ui_kit::{
    UIAdaptivePresentationControllerDelegate, UIApplication, UIBarButtonItem, UIBarButtonSystemItem,
    UINavigationController, UIPresentationController, UIView, UIViewController, UIWindowScene,
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

/// The views of a showing page, kept on the main thread. The web view is a `WKWebView`; objc2's
/// WebKit crate binds it for macOS only, so WebKit is messaged dynamically here.
struct Views {
    web: Retained<AnyObject>,
    sheet: Retained<UINavigationController>,
    _delegate: Retained<PageDelegate>,
}

thread_local! {
    /// Main thread only.
    static PAGES: RefCell<HashMap<u64, Views>> = RefCell::new(HashMap::new());
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// `WKNavigationActionPolicy`.
const ALLOW: NSInteger = 1;
const CANCEL: NSInteger = 0;
/// `WKUserScriptInjectionTimeAtDocumentStart`.
const AT_DOCUMENT_START: NSInteger = 0;

fn webkit(name: &std::ffi::CStr) -> &'static AnyClass {
    AnyClass::get(name).unwrap_or_else(|| panic!("WebKit has no {name:?}; is WebKit.framework linked?"))
}

fn text(url: Option<Retained<NSURL>>) -> String {
    url.and_then(|url| url.absoluteString()).map(|text| text.to_string()).unwrap_or_default()
}

/// The web view's current URL (main thread).
fn url_of(web: &AnyObject) -> String {
    // SAFETY: `URL` on a WKWebView, on the main thread.
    text(unsafe { msg_send![web, URL] })
}

/// The URL error codes that mean "no network": offline, DNS, unreachable, timeout, TLS.
fn network_failure(error: &NSError) -> bool {
    // SAFETY: Foundation's constant.
    let domain = unsafe { NSURLErrorDomain };
    error.domain().to_string() == domain.to_string()
        && matches!(error.code(), -1001 | -1003 | -1004 | -1005 | -1006 | -1009 | -1018 | -1020 | -2000 | -1206..=-1200)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Main-thread only, as UIKit's and
    // WebKit's delegates are. The WebKit delegate methods are defined without the Rust protocol
    // traits (macOS-only in objc2); WebKit asks `respondsToSelector:` before calling each.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TinyWebPageDelegate"]
    #[ivars = Ivars]
    struct PageDelegate;

    unsafe impl NSObjectProtocol for PageDelegate {}

    unsafe impl UIAdaptivePresentationControllerDelegate for PageDelegate {
        #[unsafe(method(presentationControllerDidDismiss:))]
        fn swiped_away(&self, _controller: &UIPresentationController) {
            self.mark_closed();
            let id = self.ivars().id;
            PAGES.with(|pages| pages.borrow_mut().remove(&id));
        }
    }

    impl PageDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(&self, web: &AnyObject, action: &AnyObject, handler: &DynBlock<dyn Fn(NSInteger)>) {
            // SAFETY: a WKNavigationAction's request and target frame.
            let (url, main) = unsafe {
                let request: Retained<NSURLRequest> = msg_send![action, request];
                let frame: Option<Retained<AnyObject>> = msg_send![action, targetFrame];
                let main: bool = match frame {
                    Some(frame) => msg_send![&*frame, isMainFrame],
                    None => true,
                };
                (text(request.URL()), main)
            };
            if !main || url.is_empty() || url == "about:blank" {
                handler.call((ALLOW,));
                return;
            }
            match (self.ivars().navigate)(&url) {
                Navigation::Allow => handler.call((ALLOW,)),
                Navigation::Cancel => handler.call((CANCEL,)),
                Navigation::Load(other) => {
                    handler.call((CANCEL,));
                    load_in(web, &other);
                }
            }
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, web: &AnyObject, _navigation: Option<&AnyObject>) {
            if let Ok(mut inner) = self.ivars().inner.lock() {
                inner.load_failed = false;
                inner.url = url_of(web);
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, web: &AnyObject, _navigation: Option<&AnyObject>) {
            if let Ok(mut inner) = self.ivars().inner.lock() {
                inner.url = url_of(web);
            }
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _web: &AnyObject, _navigation: Option<&AnyObject>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed_late(&self, _web: &AnyObject, _navigation: Option<&AnyObject>, error: &NSError) {
            self.failed(error);
        }

        /// A new-window request (`window.open`, `target=_blank`): asked about, then loaded here.
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(
            &self,
            web: &AnyObject,
            _configuration: &AnyObject,
            action: &AnyObject,
            _features: &AnyObject,
        ) -> *mut AnyObject {
            // SAFETY: a WKNavigationAction's request.
            let url = text(unsafe {
                let request: Retained<NSURLRequest> = msg_send![action, request];
                request.URL()
            });
            if !url.is_empty() {
                match (self.ivars().navigate)(&url) {
                    Navigation::Allow => load_in(web, &url),
                    Navigation::Load(other) => load_in(web, &other),
                    Navigation::Cancel => {}
                }
            }
            std::ptr::null_mut()
        }

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

fn load_in(web: &AnyObject, url: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
        let request = NSURLRequest::requestWithURL(&url);
        // SAFETY: `loadRequest:` on a WKWebView, on the main thread.
        let _: Option<Retained<AnyObject>> = unsafe { msg_send![web, loadRequest: &*request] };
    }
}

/// Takes page `id` away (main thread).
fn dismiss(id: u64) {
    if let Some(views) = PAGES.with(|pages| pages.borrow_mut().remove(&id)) {
        views.sheet.dismissViewControllerAnimated_completion(true, None);
    }
}

/// The view controller on top of the key window, to present over.
pub(super) fn top_controller(mtm: MainThreadMarker) -> Option<Retained<UIViewController>> {
    let app = UIApplication::sharedApplication(mtm);
    {
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
fn data_store(network: &str) -> Retained<AnyObject> {
    let class = webkit(c"WKWebsiteDataStore");
    if objc2::available!(ios = 17.0) {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(format!("tinyclient web sign-in {network}").as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let text = format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            u32::from_be_bytes(bytes[0..4].try_into().unwrap_or_default()),
            u16::from_be_bytes([bytes[4], bytes[5]]),
            u16::from_be_bytes([bytes[6], bytes[7]]),
            u16::from_be_bytes([bytes[8], bytes[9]]),
            bytes[10..16].iter().fold(0u64, |value, byte| value << 8 | *byte as u64),
        );
        let uuid = NSUUID::from_string(&NSString::from_str(&text)).unwrap_or_else(NSUUID::new);
        // SAFETY: iOS 17's persistent store by identifier, on the main thread.
        unsafe { msg_send![class, dataStoreForIdentifier: &*uuid] }
    } else {
        // SAFETY: the default persistent store, on the main thread.
        unsafe { msg_send![class, defaultDataStore] }
    }
}

/// Shows `page` and returns its handle at once, the page loading.
pub(super) fn open(page: WebPage) -> Result<IosPage, String> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let inner = Arc::new(Mutex::new(Inner { url: page.url.clone(), ..Inner::default() }));
    let shared = inner.clone();
    super::on_main(move |mtm| -> Result<(), String> {
        let top = top_controller(mtm).ok_or_else(|| "The sign-in page has no window to open over.".to_owned())?;
        // SAFETY: WebKit and UIKit set up on the main thread, as their APIs say; every message
        // is one WKWebView, WKWebViewConfiguration and WKUserScript document.
        unsafe {
            let configuration: Retained<AnyObject> = msg_send![webkit(c"WKWebViewConfiguration"), new];
            let store = data_store(&page.network);
            let _: () = msg_send![&*configuration, setWebsiteDataStore: &*store];
            if let Some(script) = &page.init_script {
                let script_alloc: Allocated<AnyObject> = msg_send![webkit(c"WKUserScript"), alloc];
                let script: Retained<AnyObject> = msg_send![
                    script_alloc,
                    initWithSource: &*NSString::from_str(script),
                    injectionTime: AT_DOCUMENT_START,
                    forMainFrameOnly: false
                ];
                let contents: Retained<AnyObject> = msg_send![&*configuration, userContentController];
                let _: () = msg_send![&*contents, addUserScript: &*script];
            }
            let web_alloc: Allocated<AnyObject> = msg_send![webkit(c"WKWebView"), alloc];
            let web: Retained<AnyObject> =
                msg_send![web_alloc, initWithFrame: CGRect::default(), configuration: &*configuration];
            let _: () = msg_send![&*web, setCustomUserAgent: &*NSString::from_str(&page.user_agent)];
            let delegate = PageDelegate::new(mtm, Ivars { id, navigate: page.navigate.clone(), inner: shared });
            let _: () = msg_send![&*web, setNavigationDelegate: &*delegate];
            let _: () = msg_send![&*web, setUIDelegate: &*delegate];

            let controller = UIViewController::new(mtm);
            let view: &UIView = &*(&*web as *const AnyObject as *const UIView);
            controller.setView(Some(view));
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
fn web_of(id: u64) -> Option<Retained<AnyObject>> {
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
        let url = super::on_main(move |_| web_of(id).map(|web| url_of(&web)));
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
            let block: &DynBlock<dyn Fn(*mut AnyObject, *mut NSError)> = &block;
            let _: () = unsafe { msg_send![&*web, evaluateJavaScript: &*NSString::from_str(&script), completionHandler: block] };
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
            let block: &DynBlock<dyn Fn(std::ptr::NonNull<NSArray<NSHTTPCookie>>)> = &block;
            unsafe {
                let configuration: Retained<AnyObject> = msg_send![&*web, configuration];
                let store: Retained<AnyObject> = msg_send![&*configuration, websiteDataStore];
                let cookies: Retained<AnyObject> = msg_send![&*store, httpCookieStore];
                let _: () = msg_send![&*cookies, getAllCookies: block];
            }
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
