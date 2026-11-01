//! A web sign-in driven through `protocol::web_sign_in`, the platform's web view, instead of a
//! window of this crate's own: what a phone runs. It makes the same decisions the desktop helper
//! makes in `desktop.rs` (which page, which navigations to rewrite or drop, when to read, what to
//! read, when it is over) and builds the same session line; the platform only shows the page.
//!
//! [`sign_in`] blocks its caller (one of chatd's request threads) until the user is signed in
//! or the sign-in ended, and answers the session as the helper prints it, or the helper's exit
//! code for how it ended (`CLOSED`, `LOAD_FAILED`, `BLOCKED`, `NO_TOKEN`, 6 for Teams' refusal,
//! `USAGE` for a network with no web sign-in).

use crate::teams::{self, Flow};
use crate::{blocked, hands_off, route, session, web_client, Route, Site, BLOCKED, CLOSED, DISCORD, LOAD_FAILED, NO_TOKEN, NO_TOKEN_AFTER, PAGE, SLACK, USAGE};
use protocol::web_sign_in::{Navigation, WebPage, WebPageHandle, WebSignIn};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the page is looked at, as the desktop window does.
const TICK: Duration = Duration::from_secs(1);

/// The value a page's script answered, as the JSON text the platform gives: a string unquoted,
/// anything else as it is; `None` for nothing.
pub fn js_return(value: &str) -> Option<String> {
    if value.is_empty() || value == "null" || value == "undefined" {
        return None;
    }
    serde_json::from_str(value).ok().or_else(|| Some(value.to_owned()))
}

/// What the navigation callback saw, shared with the loop.
#[derive(Default)]
struct Seen {
    /// The page the last allowed navigation went to: the `from` of the next.
    current: String,
    /// Microsoft's redirect, caught before it loaded (Teams).
    redirect: Option<String>,
}

/// The navigation policy: the desktop helper's `route`, plus Teams' redirect, which is caught.
fn navigate(seen: &Mutex<Seen>, url: &str, teams: bool) -> Navigation {
    let mut seen = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if teams && Flow::is_redirect(url) {
        seen.redirect = Some(url.to_owned());
        return Navigation::Cancel;
    }
    let from = (!seen.current.is_empty()).then(|| seen.current.clone());
    match route(url, from.as_deref()) {
        Route::Allow => {
            seen.current = url.to_owned();
            Navigation::Allow
        }
        Route::Rewrite(instead) => Navigation::Load(instead),
        Route::Block => Navigation::Cancel,
    }
}

/// Signs in to `network` (`discord`, `slack`, `teams`) in the platform's web view.
pub fn sign_in(provider: &dyn WebSignIn, network: &str) -> Result<String, i32> {
    let flow = match network {
        "teams" => Some(Flow::new()?),
        "discord" | "slack" => None,
        _ => return Err(USAGE),
    };
    let site = match network {
        "slack" => SLACK,
        "discord" => DISCORD,
        _ => Site { title: teams::TITLE, start: "", probe: teams::PROBE, read: |_| None, cookie: None, detour: None },
    };
    let start = flow.as_ref().map_or_else(|| site.start.to_owned(), Flow::start);
    let agent = crate::user_agent();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let teams = flow.is_some();
    let page = WebPage {
        network: network.to_owned(),
        title: site.title.to_owned(),
        url: start,
        user_agent: agent.clone(),
        init_script: None,
        navigate: {
            let seen = seen.clone();
            Arc::new(move |url: &str| navigate(&seen, url, teams))
        },
    };
    let mut handle = provider.open_page(page).map_err(|why| {
        tracing::warn!(network, "login: the web page could not be shown: {why}");
        LOAD_FAILED
    })?;
    let ended = drive(&mut *handle, &site, flow.as_ref(), &seen, &agent);
    handle.close();
    ended.map(|session| session.to_string())
}

/// The loop: once a second, the same looks the desktop window takes.
fn drive(page: &mut dyn WebPageHandle, site: &Site, flow: Option<&Flow>, seen: &Mutex<Seen>, agent: &str) -> Result<serde_json::Value, i32> {
    let mut refused = false;
    let mut on_client: Option<Instant> = None;
    // A Slack session read, waiting for its `d` cookie.
    let mut pending: Option<serde_json::Value> = None;
    loop {
        std::thread::sleep(TICK);
        let state = page.state();
        if state.closed {
            return Err(if refused { BLOCKED } else { CLOSED });
        }
        if state.load_failed {
            tracing::warn!(class = "network", "login failed");
            return Err(LOAD_FAILED);
        }
        if let Some(flow) = flow {
            let caught = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).redirect.take();
            for url in caught.iter().chain(Some(&state.url)) {
                if let Some(ending) = flow.navigated(url) {
                    return ending;
                }
            }
        }
        if let (Some(wanted), Some(probed)) = (site.cookie, pending.as_ref()) {
            if let Some(cookie) = page.cookie("slack.com", wanted) {
                return Ok(session(probed.clone(), Some(cookie), agent));
            }
        }
        let at = state.url.as_str();
        if let Some((path, instead)) = site.detour {
            if at.contains(path) {
                page.load(instead);
                continue;
            }
        }
        if hands_off(at) {
            continue;
        }
        if web_client(at) && on_client.get_or_insert_with(Instant::now).elapsed() >= NO_TOKEN_AFTER {
            tracing::warn!("login: signed in to Slack, but the web view holds no token to read");
            return Err(NO_TOKEN);
        }
        if let Some(found) = page.evaluate(PAGE).as_deref().and_then(js_return) {
            if let Ok(shown) = serde_json::from_str::<serde_json::Value>(&found) {
                if !refused && blocked(shown["url"].as_str().unwrap_or(""), shown["text"].as_str().unwrap_or("")) {
                    refused = true;
                    tracing::warn!("login: the organization does not allow this sign-in");
                }
            }
        }
        let Some(found) = page.evaluate(site.probe).as_deref().and_then(js_return) else {
            continue;
        };
        if let Some(flow) = flow {
            if let Some(ending) = flow.read(&found) {
                return ending;
            }
            continue;
        }
        let Some(probed) = (site.read)(&found) else {
            continue;
        };
        if site.cookie.is_none() {
            return Ok(session(probed, None, agent));
        }
        pending = Some(probed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::web_sign_in::{AuthHandle, AuthRequest, PageState};

    /// A web view that is a script: the URL it is on, and what each probe answers.
    struct Scripted {
        url: String,
        probe: Option<String>,
        cookie: Option<String>,
        closed: bool,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl WebPageHandle for Scripted {
        fn state(&mut self) -> PageState {
            PageState { url: self.url.clone(), closed: self.closed, load_failed: false }
        }
        fn evaluate(&mut self, script: &str) -> Option<String> {
            if script == PAGE {
                return Some(serde_json::to_string(&serde_json::json!({"url": self.url, "text": ""}).to_string()).unwrap());
            }
            self.probe.clone()
        }
        fn cookie(&mut self, domain: &str, name: &str) -> Option<String> {
            self.log.lock().unwrap().push(format!("cookie {domain} {name}"));
            self.cookie.clone()
        }
        fn load(&mut self, url: &str) {
            self.url = url.to_owned();
        }
        fn close(&mut self) {
            self.log.lock().unwrap().push("close".to_owned());
        }
    }

    struct Provider {
        page: Mutex<Option<Scripted>>,
        opened: Mutex<Vec<(String, String)>>,
        navigate: Mutex<Option<Arc<dyn Fn(&str) -> Navigation + Send + Sync>>>,
    }

    impl WebSignIn for Provider {
        fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String> {
            self.opened.lock().unwrap().push((page.network.clone(), page.url.clone()));
            *self.navigate.lock().unwrap() = Some(page.navigate.clone());
            Ok(Box::new(self.page.lock().unwrap().take().ok_or("opened twice")?))
        }
        fn open_auth(&self, _: AuthRequest) -> Result<Box<dyn AuthHandle>, String> {
            Err("no".to_owned())
        }
    }

    fn provider(page: Scripted) -> Provider {
        Provider { page: Mutex::new(Some(page)), opened: Mutex::default(), navigate: Mutex::default() }
    }

    #[test]
    fn discord_is_read_from_its_probe_and_the_page_is_closed() {
        let log = Arc::default();
        let page = Scripted { url: "https://discord.com/channels/@me".into(), probe: Some(r#""{\"token\":\"abc\"}""#.into()), cookie: None, closed: false, log: Arc::clone(&log) };
        let provider = provider(page);
        let line = sign_in(&provider, "discord").unwrap();
        let session: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(session["token"], "abc");
        assert_eq!(session["user_agent"], crate::user_agent());
        assert_eq!(provider.opened.lock().unwrap()[0], ("discord".to_owned(), DISCORD.start.to_owned()));
        assert_eq!(log.lock().unwrap().last().map(String::as_str), Some("close"));
    }

    #[test]
    fn slack_waits_for_its_cookie_and_its_links_are_routed() {
        let config = r#"{"teams":{"T1":{"name":"Acme","url":"https://acme.slack.com/","token":"xoxc-1"}},"lastActiveTeamId":"T1"}"#;
        let log = Arc::default();
        let page = Scripted { url: "https://app.slack.com/client/T1".into(), probe: Some(serde_json::to_string(config).unwrap()), cookie: Some("xoxd-9".into()), closed: false, log: Arc::clone(&log) };
        let provider = provider(page);
        let line = sign_in(&provider, "slack").unwrap();
        let session: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!((session["cookie"].as_str(), session["teams"][0]["token"].as_str()), (Some("xoxd-9"), Some("xoxc-1")));
        assert!(log.lock().unwrap().contains(&"cookie slack.com d".to_owned()));
        let navigate = provider.navigate.lock().unwrap().clone().unwrap();
        assert_eq!(navigate("https://app.slack.com/ssb/redirect?team=T0G1MMR"), Navigation::Load("https://app.slack.com/client/T0G1MMR".into()));
        assert_eq!(navigate("https://app.slack.com/client/T0G1MMR"), Navigation::Allow);
        assert_eq!(navigate("slack://open?team=T0G1MMR"), Navigation::Cancel);
    }

    #[test]
    fn closing_the_page_is_the_helpers_closed_and_unknown_networks_are_refused() {
        let page = Scripted { url: "https://discord.com/login".into(), probe: None, cookie: None, closed: true, log: Arc::default() };
        assert_eq!(sign_in(&provider(page), "discord"), Err(CLOSED));
        let page = Scripted { url: String::new(), probe: None, cookie: None, closed: false, log: Arc::default() };
        assert_eq!(sign_in(&provider(page), "messages"), Err(USAGE));
    }

    #[test]
    fn teams_catches_microsofts_redirect_and_a_refusal_ends_it() {
        let page = Scripted { url: "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize".into(), probe: None, cookie: None, closed: false, log: Arc::default() };
        let provider = provider(page);
        let worker = std::thread::scope(|scope| {
            let running = scope.spawn(|| sign_in(&provider, "teams"));
            let deadline = Instant::now() + Duration::from_secs(5);
            let navigate = loop {
                if let Some(navigate) = provider.navigate.lock().unwrap().clone() {
                    break navigate;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            };
            let redirect = "https://login.microsoftonline.com/common/oauth2/nativeclient?error=access_denied&error_description=AADSTS53003%3A+blocked";
            assert_eq!(navigate(redirect), Navigation::Cancel);
            running.join().unwrap()
        });
        assert_eq!(worker, Err(BLOCKED));
        assert!(provider.opened.lock().unwrap()[0].1.contains("code_challenge_method=S256"));
    }
}
