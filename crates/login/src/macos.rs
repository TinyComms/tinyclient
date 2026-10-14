//! macOS sign-in window (AppKit + WKWebView).

use crate::{
    blocked, finish, load_failed, origin, route, session, web_client, Route, Site, BLOCKED, CLOSED,
    LOAD_FAILED, NO_TOKEN, NO_TOKEN_AFTER, PAGE, SIZE,
};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSScreen, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSError, NSHTTPCookie, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSTimer, NSURLRequest, NSURL, NSUUID,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use crate::teams;

fn open_here(web: &WKWebView, url: &str) {
    let url = NSURL::URLWithString(&NSString::from_str(url)).expect("a valid url");
    unsafe { web.loadRequest(&NSURLRequest::requestWithURL(&url)) };
}

fn page_url(web: &WKWebView) -> Option<String> {
    Some(unsafe { web.URL() }?.absoluteString()?.to_string())
}

fn action_url(action: &WKNavigationAction) -> Option<String> {
    let url = unsafe { action.request().URL() }?;
    let text = url.absoluteString()?.to_string();
    (text != "about:blank" && !text.is_empty()).then_some(text)
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct WebDelegate;

    unsafe impl NSObjectProtocol for WebDelegate {}

    impl WebDelegate {
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> *mut WKWebView {
            if let Some(url) = action_url(navigation_action) {
                teams::navigated(&url);
                match route(&url, page_url(web_view).as_deref()) {
                    Route::Allow => open_here(web_view, &url),
                    Route::Rewrite(instead) => open_here(web_view, &instead),
                    Route::Block => {}
                }
            }
            std::ptr::null_mut()
        }
    }

    unsafe impl WKNavigationDelegate for WebDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            web_view: &WKWebView,
            navigation_action: &WKNavigationAction,
            decision_handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let route = action_url(navigation_action).map_or(Route::Allow, |url| {
                teams::navigated(&url);
                route(&url, page_url(web_view).as_deref())
            });
            match route {
                Route::Allow => decision_handler.call((WKNavigationActionPolicy::Allow,)),
                Route::Rewrite(instead) => {
                    decision_handler.call((WKNavigationActionPolicy::Cancel,));
                    open_here(web_view, &instead);
                }
                Route::Block => decision_handler.call((WKNavigationActionPolicy::Cancel,)),
            }
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            if load_failed(&error.domain().to_string(), error.code()) {
                eprintln!("tinychat-login: the page did not load: {}", error.localizedDescription());
                std::process::exit(LOAD_FAILED);
            }
        }
    }
);

impl WebDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

pub fn run(site: Site, at: Option<(f64, f64)>) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(SIZE.0, SIZE.1));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable;
    let window: Retained<NSWindow> = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(site.title));
    window.center();
    if let Some(at) = at {
        let screens = NSScreen::screens(mtm);
        let main_height = screens.iter().next().map_or(0., |main| main.frame().size.height);
        let point = NSPoint::new(at.0, main_height - at.1);
        let inside = |frame: NSRect| {
            (frame.origin.x..frame.origin.x + frame.size.width).contains(&point.x)
                && (frame.origin.y..frame.origin.y + frame.size.height).contains(&point.y)
        };
        let visible = screens.iter().find(|screen| inside(screen.frame())).map(|screen| {
            let visible = screen.visibleFrame();
            (visible.origin.x, visible.origin.y, visible.size.width, visible.size.height)
        });
        let (x, y) = origin(at, SIZE, main_height, visible);
        window.setFrameOrigin(NSPoint::new(x, y));
    }

    let store_id = NSUUID::initWithUUIDString(
        NSUUID::alloc(),
        &NSString::from_str("c0a1e5b2-7d44-4f18-9a6e-3b8d1f0c6e27"),
    )
    .expect("uuid");
    let store = unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&store_id, mtm) };
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe { config.setWebsiteDataStore(&store) };
    let web =
        unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config) };
    let agent = crate::user_agent();
    unsafe { web.setCustomUserAgent(Some(&NSString::from_str(&agent))) };
    let delegate = WebDelegate::new(mtm);
    unsafe {
        let _: () = msg_send![&web, setUIDelegate: &*delegate];
        web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    let load = |web: &WKWebView, url: &str| unsafe {
        let url = NSURL::URLWithString(&NSString::from_str(url)).expect("a valid url");
        web.loadRequest(&NSURLRequest::requestWithURL(&url));
    };
    load(&web, site.start);
    window.setContentView(Some(&web));
    window.makeKeyAndOrderFront(None);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    let cookies = unsafe { store.httpCookieStore() };
    let probe = NSString::from_str(site.probe);
    let page = NSString::from_str(PAGE);
    let refused = Rc::new(Cell::new(false));
    let on_client: Cell<Option<Instant>> = Cell::new(None);
    let tick = RcBlock::new(move |_: NonNull<NSTimer>| {
        if !window.isVisible() {
            std::process::exit(if refused.get() { BLOCKED } else { CLOSED });
        }
        let at = page_url(&web).unwrap_or_default();
        if let Some((path, instead)) = site.detour {
            if at.contains(path) {
                load(&web, instead);
                return;
            }
        }
        on_client.set(web_client(&at).then(|| on_client.get().unwrap_or_else(Instant::now)));
        if on_client.get().is_some_and(|since| since.elapsed() >= NO_TOKEN_AFTER) {
            eprintln!(
                "tinychat-login: signed in to Slack, but the browser holds no token to read after \
                 {} seconds; Enterprise Grid organizations are known to do this",
                NO_TOKEN_AFTER.as_secs()
            );
            std::process::exit(NO_TOKEN);
        }
        let refused = refused.clone();
        let read = RcBlock::new(move |value: *mut AnyObject, _: *mut NSError| {
            let text = unsafe { value.as_ref() }.and_then(|value| value.downcast_ref::<NSString>());
            let page = text.and_then(|text| serde_json::from_str::<serde_json::Value>(&text.to_string()).ok());
            if let Some(page) = page {
                let (url, text) = (page["url"].as_str().unwrap_or(""), page["text"].as_str().unwrap_or(""));
                if blocked(url, text) && !refused.replace(true) {
                    eprintln!("tinychat-login: the organization does not allow this sign-in");
                }
            }
        });
        unsafe { web.evaluateJavaScript_completionHandler(&page, Some(&read)) };
        let cookies = cookies.clone();
        let agent = agent.clone();
        let probed = RcBlock::new(move |value: *mut AnyObject, _: *mut NSError| {
            let text = unsafe { value.as_ref() }.and_then(|value| value.downcast_ref::<NSString>());
            let Some(text) = text else {
                return;
            };
            let Some(probed) = (site.read)(&text.to_string()) else {
                return;
            };
            let Some(wanted) = site.cookie else {
                finish(session(probed, None, &agent));
            };
            let agent = agent.clone();
            let found = RcBlock::new(move |all: NonNull<NSArray<NSHTTPCookie>>| {
                let all = unsafe { all.as_ref() };
                let cookie = all
                    .iter()
                    .find(|cookie| cookie.name().to_string() == wanted)
                    .map(|cookie| cookie.value().to_string());
                if cookie.is_some() {
                    finish(session(probed.clone(), cookie, &agent));
                }
            });
            unsafe { cookies.getAllCookies(&found) };
        });
        unsafe { web.evaluateJavaScript_completionHandler(&probe, Some(&probed)) };
    });
    let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1., true, &tick) };
    app.run();
}
