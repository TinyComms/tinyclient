//! The web sign-in on Android: Discord, Slack and Teams in `WebSignInActivity` (an
//! `android.webkit.WebView`), a Tiny server's OIDC page in a Custom Tab (`AuthSession`).

use super::jvm::{self, AUTH, WEB};
use super::natives::{next_id, AUTHS, PAGES};
use jni::objects::{GlobalRef, JObject, JValue};
use protocol::web_sign_in::{AuthHandle, AuthRequest, PageState, WebPage, WebPageHandle};

pub struct Page {
    id: i64,
    session: GlobalRef,
    closed: bool,
}

pub fn open_page(page: WebPage) -> Result<Box<dyn WebPageHandle>, String> {
    let id = next_id();
    PAGES.put(id, page.navigate.clone());
    let opened = jvm::call("open the sign-in page", |env| {
        let network = env.new_string(&page.network)?;
        let title = env.new_string(&page.title)?;
        let url = env.new_string(&page.url)?;
        let agent = env.new_string(&page.user_agent)?;
        let script = match &page.init_script {
            Some(script) => JObject::from(env.new_string(script)?),
            None => JObject::null(),
        };
        let made = jvm::call_static(
            env,
            WEB,
            "open",
            "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ldev/tinyclient/app/WebSession;",
            &[JValue::Long(id), JValue::Object(&network), JValue::Object(&title), JValue::Object(&url), JValue::Object(&agent), JValue::Object(&script)],
        )?
        .l()?;
        if made.is_null() {
            return Ok(None);
        }
        Ok(Some(env.new_global_ref(made)?))
    });
    match opened {
        Ok(Some(session)) => Ok(Box::new(Page { id, session, closed: false })),
        Ok(None) => {
            PAGES.take(id);
            Err(format!("The {} sign-in page couldn't be shown.", page.title))
        }
        Err(words) => {
            PAGES.take(id);
            Err(words)
        }
    }
}

impl Page {
    fn text(&self, what: &str, method: &str, sig: &str, args: &[&str]) -> Option<String> {
        let session = self.session.clone();
        jvm::call(what, |env| {
            let mut strings = Vec::with_capacity(args.len());
            for arg in args {
                strings.push(env.new_string(arg)?);
            }
            let values: Vec<JValue> = strings.iter().map(|s| JValue::Object(s)).collect();
            let out = env.call_method(session.as_obj(), method, sig, &values)?.l()?;
            jvm::string(env, out)
        })
        .ok()
        .flatten()
    }
}

impl WebPageHandle for Page {
    fn state(&mut self) -> PageState {
        let Some(state) = self.text("read the sign-in page", "state", "()Ljava/lang/String;", &[]) else {
            return PageState { url: String::new(), closed: false, load_failed: false };
        };
        super::wire::page_state(&state)
    }

    fn evaluate(&mut self, script: &str) -> Option<String> {
        self.text("run the sign-in page's script", "evaluate", "(Ljava/lang/String;)Ljava/lang/String;", &[script])
    }

    fn cookie(&mut self, domain: &str, name: &str) -> Option<String> {
        self.text("read the sign-in page's cookies", "cookie", "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;", &[domain, name])
    }

    fn load(&mut self, url: &str) {
        let session = self.session.clone();
        let _ = jvm::call("load the sign-in page", |env| {
            let url = env.new_string(url)?;
            env.call_method(session.as_obj(), "load", "(Ljava/lang/String;)V", &[JValue::Object(&url)]).map(|_| ())
        });
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let session = self.session.clone();
        let _ = jvm::call("close the sign-in page", |env| env.call_method(session.as_obj(), "close", "()V", &[]).map(|_| ()));
        PAGES.take(self.id);
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct Auth {
    id: i64,
    session: GlobalRef,
    done: bool,
}

pub fn open_auth(auth: AuthRequest) -> Result<Box<dyn AuthHandle>, String> {
    let id = next_id();
    AUTHS.put(id, auth.redirected.clone());
    let opened = jvm::call("open the sign-in page", |env| {
        let url = env.new_string(&auth.url)?;
        let callback = env.new_string(&auth.callback)?;
        let made = jvm::call_static(env, AUTH, "open", "(JLjava/lang/String;Ljava/lang/String;)Ldev/tinyclient/app/AuthSession;", &[JValue::Long(id), JValue::Object(&url), JValue::Object(&callback)])?.l()?;
        if made.is_null() {
            return Ok(None);
        }
        Ok(Some(env.new_global_ref(made)?))
    });
    match opened {
        Ok(Some(session)) => Ok(Box::new(Auth { id, session, done: false })),
        Ok(None) => {
            AUTHS.take(id);
            Err("No browser on this phone can show the sign-in page.".to_owned())
        }
        Err(words) => {
            AUTHS.take(id);
            Err(words)
        }
    }
}

impl AuthHandle for Auth {
    fn closed(&mut self) -> bool {
        let session = self.session.clone();
        jvm::call("read the sign-in page", |env| env.call_method(session.as_obj(), "isClosed", "()Z", &[])?.z()).unwrap_or(false)
    }

    fn close(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let session = self.session.clone();
        let _ = jvm::call("close the sign-in page", |env| env.call_method(session.as_obj(), "close", "()V", &[]).map(|_| ()));
        AUTHS.take(self.id);
    }
}

impl Drop for Auth {
    fn drop(&mut self) {
        self.close();
    }
}
