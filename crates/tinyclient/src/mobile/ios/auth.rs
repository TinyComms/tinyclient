//! A Tiny server's sign-in: its OIDC page (Pocket ID, passkeys) in `ASWebAuthenticationSession`.
//!
//! chatd's loopback listener stays the only receiver (docs/mobile-platform.md §3): the page
//! redirects the session's browser to `http://127.0.0.1:<port>/callback`, which chatd, running
//! in this app, answers. The session is given a callback scheme nothing ever uses; if it does
//! end on a URL anyway, that URL goes to `AuthRequest::redirected`. A user's Cancel marks the
//! handle closed.

// `initWithURL:callbackURLScheme:` is deprecated from iOS 17.4 for the callback variant, which
// takes https or a custom scheme only; the loopback redirect needs neither, and iOS 16 needs this.
#![allow(deprecated)]

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_authentication_services::{ASWebAuthenticationPresentationContextProviding, ASWebAuthenticationSession};
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_ui_kit::{UIApplication, UIWindow, UIWindowScene};
use protocol::web_sign_in::{AuthHandle, AuthRequest};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The scheme the session waits for. Nothing redirects to it; chatd closes the session.
const NEVER: &str = "dev.tinyclient.never";

/// `ASWebAuthenticationSessionErrorCodeCanceledLogin`.
const CANCELED: isize = 1;

define_class!(
    // SAFETY: NSObject has no subclassing requirements; main-thread only, as the protocol is.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TinyAuthAnchor"]
    struct Anchor;

    unsafe impl NSObjectProtocol for Anchor {}

    unsafe impl ASWebAuthenticationPresentationContextProviding for Anchor {}

    impl Anchor {
        /// The protocol's one method; objc2 declares it for macOS only, so it is defined here.
        #[unsafe(method(presentationAnchorForWebAuthenticationSession:))]
        fn anchor(&self, _session: &ASWebAuthenticationSession) -> *mut UIWindow {
            // UIKit keeps the key window alive; it is returned at +0, as `method_family = none` says.
            key_window(self.mtm()).map(|window| Retained::as_ptr(&window) as *mut UIWindow).unwrap_or(std::ptr::null_mut())
        }
    }
);

fn key_window(mtm: MainThreadMarker) -> Option<Retained<UIWindow>> {
    let app = UIApplication::sharedApplication(mtm);
    app.connectedScenes().iter().filter_map(|scene| scene.downcast::<UIWindowScene>().ok()).find_map(|scene| {
        let windows = scene.windows();
        windows.iter().find(|window| window.isKeyWindow()).or_else(|| windows.firstObject())
    })
}

/// A showing session, kept on the main thread.
struct Showing {
    session: Retained<ASWebAuthenticationSession>,
    _anchor: Retained<Anchor>,
}

thread_local! {
    /// Main thread only.
    static SESSIONS: RefCell<HashMap<u64, Showing>> = RefCell::new(HashMap::new());
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Shows `auth.url` and returns at once.
pub(super) fn open(auth: AuthRequest) -> Result<IosAuth, String> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let closed = Arc::new(AtomicBool::new(false));
    let flag = closed.clone();
    super::on_main(move |mtm| -> Result<(), String> {
        let url = NSURL::URLWithString(&NSString::from_str(&auth.url))
            .ok_or_else(|| "The sign-in page's address is not valid.".to_owned())?;
        let redirected = auth.redirected.clone();
        let completion = block2::RcBlock::new(move |callback: *mut NSURL, error: *mut NSError| {
            // SAFETY: the session's URL and error, valid for the call.
            let (callback, error) = unsafe { (callback.as_ref(), error.as_ref()) };
            if let Some(url) = callback.and_then(|url| url.absoluteString()) {
                redirected(url.to_string());
            } else if error.is_some_and(|error| error.code() == CANCELED) {
                log::info!("tinyclient: Tiny sign-in dismissed");
                flag.store(true, Ordering::Relaxed);
            }
            let finished = id;
            super::on_main_later(move |_| {
                SESSIONS.with(|sessions| sessions.borrow_mut().remove(&finished));
            });
        });
        let anchor: Retained<Anchor> = {
            let this = Anchor::alloc(mtm).set_ivars(());
            // SAFETY: NSObject's init.
            unsafe { msg_send![super(this), init] }
        };
        // SAFETY: AuthenticationServices' documented use, on the main thread; the session copies
        // the completion block and keeps the provider weakly (held in `SESSIONS`).
        unsafe {
            let session = ASWebAuthenticationSession::initWithURL_callbackURLScheme_completionHandler(
                ASWebAuthenticationSession::alloc(),
                &url,
                Some(&NSString::from_str(NEVER)),
                block2::RcBlock::as_ptr(&completion),
            );
            session.setPresentationContextProvider(Some(ProtocolObject::from_ref(&*anchor)));
            session.setPrefersEphemeralWebBrowserSession(false);
            if !session.start() {
                return Err("The sign-in page could not be opened.".to_owned());
            }
            log::info!("tinyclient: Tiny sign-in session started");
            SESSIONS.with(|sessions| sessions.borrow_mut().insert(id, Showing { session, _anchor: anchor }));
        }
        Ok(())
    })?;
    Ok(IosAuth { id, closed })
}

/// The handle chatd holds.
pub(super) struct IosAuth {
    id: u64,
    closed: Arc<AtomicBool>,
}

impl AuthHandle for IosAuth {
    fn closed(&mut self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    fn close(&mut self) {
        let id = self.id;
        super::on_main(move |_| {
            if let Some(showing) = SESSIONS.with(|sessions| sessions.borrow_mut().remove(&id)) {
                // SAFETY: cancelling the session this module started, on the main thread.
                unsafe { showing.session.cancel() };
            }
        });
    }
}
