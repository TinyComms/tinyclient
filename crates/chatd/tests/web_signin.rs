//! A phone's web sign-in: with a `protocol::web_sign_in` provider registered, chatd shows the
//! network's page through it (no `tinyclient-login` helper), reads the session the way the
//! helper does (`login::web`), and goes on to keep it. In a scratch directory nothing is kept,
//! so the keeping is seen as its refusal, after the session was made.

use protocol::web_sign_in::{AuthHandle, AuthRequest, PageState, WebPage, WebPageHandle, WebSignIn};
use protocol::{chat_sock_path_in, read_frame, write_frame, ChatStream, ErrorCode, Network, Request, Response};
use std::sync::{Arc, Mutex};

/// Discord's page, signed in: the probe answers a token. `closed` makes it a page the user closed.
struct Page {
    closed: bool,
}

impl WebPageHandle for Page {
    fn state(&mut self) -> PageState {
        PageState { url: "https://discord.com/channels/@me".to_owned(), closed: self.closed, load_failed: false }
    }
    fn evaluate(&mut self, script: &str) -> Option<String> {
        (script.contains("localStorage.getItem('token')")).then(|| serde_json::to_string(r#"{"token":"tok-123"}"#).unwrap())
    }
    fn cookie(&mut self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn load(&mut self, _: &str) {}
    fn close(&mut self) {}
}

#[derive(Default)]
struct Phone {
    opened: Mutex<Vec<(String, String, String)>>,
    close_next: Mutex<bool>,
}

impl WebSignIn for Phone {
    fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String> {
        self.opened.lock().unwrap().push((page.network, page.url, page.user_agent));
        Ok(Box::new(Page { closed: std::mem::take(&mut *self.close_next.lock().unwrap()) }))
    }
    fn open_auth(&self, _: AuthRequest) -> Result<Box<dyn AuthHandle>, String> {
        Err("not this test".to_owned())
    }
}

#[test]
fn a_registered_web_page_signs_in_without_the_helper_and_the_session_goes_to_be_kept() {
    let dir = std::env::temp_dir().join(format!("chatd-web-signin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("TINYCLIENT_DIR", &dir);
    // A helper that cannot run, to be sure it is not what signs in.
    std::env::set_var("TINYCLIENT_LOGIN", dir.join("no-such-helper"));
    let phone = Arc::new(Phone::default());
    protocol::web_sign_in::register(phone.clone());

    let core = chatd::start_in_process(dir.clone()).expect("the core starts");
    let mut stream = ChatStream::connect(&chat_sock_path_in(&dir)).expect("the socket answers");
    write_frame(&mut stream, &Request::SignIn { network: Network::Discord, at: None }).unwrap();
    match read_frame::<Response>(&mut stream).unwrap() {
        // The session was made from the page and handed on to be kept; a scratch directory
        // keeps nothing and says so. Any other ending means the page did not sign in.
        Response::Error { code: ErrorCode::SignIn, detail } => {
            assert!(detail.contains("not kept while TINYCLIENT_DIR is set"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    let opened = phone.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert_eq!((opened[0].0.as_str(), opened[0].1.as_str()), ("discord", "https://discord.com/login"));
    assert!(!opened[0].2.is_empty(), "the page is given the browser the session will name");

    // The user closing the page is the helper's "closed", in the same words.
    *phone.close_next.lock().unwrap() = true;
    write_frame(&mut stream, &Request::SignIn { network: Network::Discord, at: None }).unwrap();
    match read_frame::<Response>(&mut stream).unwrap() {
        Response::Error { code: ErrorCode::SignIn, detail } => assert_eq!(detail, "The sign-in window was closed."),
        other => panic!("{other:?}"),
    }
    protocol::web_sign_in::unregister();
    drop(core);
}
