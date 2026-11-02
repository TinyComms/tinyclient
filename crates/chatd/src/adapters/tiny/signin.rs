//! Signing in to a Tiny server, in the system browser. A new person pastes one code: it carries
//! the server and the invite. Someone who already has an account types the host. Either way chatd
//! asks the server for its provider (`GET /v1/auth/config`), listens on 127.0.0.1, makes a PKCE
//! pair and a state, asks the server where to send the browser (`POST /v1/auth/start`, with the
//! invite when there is one), and opens it there. The browser comes back once, to this computer,
//! with a one-time code; chatd trades it and its verifier for a session (`POST /v1/auth/token`)
//! and keeps that exactly as a password once did. No name, email or password is asked here: the
//! provider has them.
//!
//! Nothing is kept until that answer is 200. The invite, the codes, the verifier, the state, and
//! the bearer are not logged. One sign-in at a time, in memory; closing the sheet drops it and
//! closes the listener.

use super::browser::{self, Back, Opener};
use super::{auth_config, auth_start, auth_token, Fault};
use crate::hub::Hub;
use protocol::{AccountKey, Change, Network, SignInInput, SignInProblem, SignInStep};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Keeps a finished session and starts its account.
pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

/// How long the browser has to come back.
const PATIENCE: Duration = Duration::from_secs(5 * 60);

/// The browser a sign-in opens, and how long it waits for it. Tests bring their own.
#[derive(Clone)]
pub(crate) struct Browser {
    pub open: Opener,
    pub patience: Duration,
}

impl Browser {
    fn system() -> Browser {
        Browser { open: browser::system(), patience: PATIENCE }
    }
}

struct Flow {
    hub: Arc<Hub>,
    server: String,
    /// The invite token, bare. Empty when signing in to an account that already exists.
    code: String,
    /// The address was typed before the code. Back from that code returns to the address.
    address_first: bool,
    /// The provider's name, as the server gave it.
    provider: String,
    step: SignInStep,
    adopt: Arc<Adopt>,
    browser: Browser,
    /// The server the person was last told is reached over plain http, off their own network. The
    /// same server asked for again is their yes; any other starts the question over.
    warned: String,
    /// They said yes to `server`. It goes into the session, so that the session may be used.
    plain_http_ok: bool,
    /// The browser is out: the page it was sent to, and the switch that stops listening for it.
    out: Option<Out>,
}

struct Out {
    authorize_url: String,
    stop: Arc<AtomicBool>,
}

static FLOW: Mutex<Option<Flow>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Flow>> {
    FLOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An http or https origin. A host with no scheme is https, except a machine on this computer,
/// which is http. A path, query, or fragment is left off. A user in the address is refused.
pub(crate) fn base_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
        return None;
    }
    let (given, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, raw),
    };
    if given.is_some_and(|scheme| {
        !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https")
    }) {
        return None;
    }
    if rest.contains(['@', '\\']) || rest.is_empty() {
        return None;
    }
    let rest = rest.split_once('#').map(|(rest, _)| rest).unwrap_or(rest);
    let rest = rest.split_once('?').map(|(rest, _)| rest).unwrap_or(rest);
    let rest = rest.split_once('/').map(|(rest, _)| rest).unwrap_or(rest);
    if rest.is_empty() {
        return None;
    }
    let ok = if let Some(v6) = rest.strip_prefix('[') {
        let (addr, after) = v6.split_once(']')?;
        !addr.is_empty()
            && addr
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
            && match after {
                "" => true,
                port => port_ok(port.strip_prefix(':')?),
            }
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
                host_ok(host) && port_ok(port)
            }
            Some(_) => false,
            None => host_ok(rest),
        }
    };
    if !ok {
        return None;
    }
    let scheme = match given {
        Some(scheme) => scheme.to_ascii_lowercase(),
        None if local_origin(rest) => "http".to_owned(),
        None => "https".to_owned(),
    };
    Some(format!("{scheme}://{rest}"))
}

fn local_origin(rest: &str) -> bool {
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split_once(']').map(|(addr, _)| addr).unwrap_or("")
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
            _ => rest,
        }
    };
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "0.0.0.0"
}

fn host_ok(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with(['.', '-'])
        && !host.ends_with('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn port_ok(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|port| port > 0)
}

/// A server token on its own: no origin wrapped around it.
fn bare_token(raw: &str) -> bool {
    !raw.is_empty() && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What to do to go on past the plain-http warning.
const CONFIRM_PLAIN_HTTP: &str = "To go on anyway, continue again.";
const OVER: &str = "That sign-in is over. Start again.";
const DENIED: &str = "The sign-in was turned down in your browser.";
const FAILED: &str = "The server couldn't finish the sign-in. Try again.";
const TIMED_OUT: &str = "Your browser didn't come back within 5 minutes. Try again.";
const NO_LISTEN: &str = "tinyclient couldn't listen for your browser on this computer.";
const BAD_PAGE: &str = "The server sent the browser somewhere tinyclient won't open.";

/// Begins a sign-in, or with `input` answers the step on show.
pub fn step(hub: &Arc<Hub>, input: Option<SignInInput>, adopt: Adopt) -> Result<SignInStep, String> {
    step_with(hub, input, adopt, Browser::system)
}

/// `step`, with the browser a new sign-in will open.
pub(crate) fn step_with(
    hub: &Arc<Hub>,
    input: Option<SignInInput>,
    adopt: Adopt,
    browser: impl FnOnce() -> Browser,
) -> Result<SignInStep, String> {
    let mut held = current();
    let Some(input) = input else {
        let flow = held.get_or_insert_with(|| Flow {
            hub: hub.clone(),
            server: String::new(),
            code: String::new(),
            address_first: false,
            provider: String::new(),
            step: SignInStep::Invite {
                server: String::new(),
                problem: None,
            },
            adopt: Arc::new(adopt),
            browser: browser(),
            warned: String::new(),
            plain_http_ok: false,
            out: None,
        });
        return Ok(flow.step.clone());
    };
    let Some(flow) = held.as_mut() else {
        return Ok(SignInStep::Failed {
            reason: OVER.to_owned(),
        });
    };
    let next = flow.answer(input);
    // Opening the system browser can block on a cold start. Never hold FLOW while
    // doing it: the callback and cancellation must be able to finish meanwhile.
    let opening = if matches!(next, SignInStep::Browser { waiting: true, .. }) {
        flow.out.as_ref().map(|out| (flow.browser.open.clone(), out.authorize_url.clone(), out.stop.clone()))
    } else {
        None
    };
    if matches!(next, SignInStep::Done { .. } | SignInStep::Failed { .. }) {
        if let Some(flow) = held.take() {
            flow.stop();
        }
    }
    drop(held);
    if let Some((open, url, stop)) = opening {
        let problem = open(&url).err().map(|reason| SignInProblem::Other { reason });
        let mut held = current();
        if let Some(flow) = held.as_mut() {
            if !stop.load(Ordering::Relaxed)
                && flow.out.as_ref().is_some_and(|out| Arc::ptr_eq(&out.stop, &stop))
            {
                flow.step = flow.browser_step(true, problem);
                return Ok(flow.step.clone());
            }
        }
    }
    Ok(next)
}

/// The user closed the sheet. Nothing was kept, and the browser is no longer listened for.
pub fn cancel() {
    if let Some(flow) = current().take() {
        flow.stop();
    }
}

impl Flow {
    fn answer(&mut self, input: SignInInput) -> SignInStep {
        let step = self.step.clone();
        let next = match (step, input) {
            (SignInStep::Server { .. }, SignInInput::Server { url }) => match base_url(&url) {
                Some(server) => {
                    self.server = server;
                    self.code.clear();
                    self.address_first = true;
                    self.begin_browser()
                }
                None => SignInStep::Server {
                    problem: Some(SignInProblem::UrlInvalid),
                },
            },
            (SignInStep::Server { .. }, SignInInput::Back) => self.fresh_code(),
            (SignInStep::Server { .. }, SignInInput::HaveCode { url }) if url.trim().is_empty() => self.fresh_code(),
            (SignInStep::Server { .. }, SignInInput::HaveCode { url }) => match base_url(&url) {
                Some(server) => {
                    self.server = server.clone();
                    self.code.clear();
                    self.address_first = true;
                    SignInStep::Invite {
                        server,
                        problem: None,
                    }
                }
                None => SignInStep::Server {
                    problem: Some(SignInProblem::UrlInvalid),
                },
            },
            (SignInStep::Invite { .. }, SignInInput::HaveCode { url }) if url.trim().is_empty() => {
                self.code.clear();
                SignInStep::Server { problem: None }
            }
            (SignInStep::Invite { .. }, SignInInput::Invite { code }) => self.take_code(&code),
            (SignInStep::Invite { .. }, SignInInput::Back) => {
                if self.address_first {
                    self.code.clear();
                    SignInStep::Server { problem: None }
                } else {
                    self.fresh_code()
                }
            }
            (SignInStep::Browser { waiting: true, .. }, SignInInput::OpenBrowser) => self.open_again(),
            (SignInStep::Browser { .. }, SignInInput::OpenBrowser) => self.begin_browser(),
            (SignInStep::Browser { .. }, SignInInput::Back) => {
                self.stop();
                self.asking(None)
            }
            _ => self.step.clone(),
        };
        self.step = next.clone();
        next
    }

    /// The step that asked for what led to the browser: the code, or the address.
    fn asking(&mut self, problem: Option<SignInProblem>) -> SignInStep {
        if self.code.is_empty() {
            return SignInStep::Server { problem };
        }
        if !self.address_first {
            // The pasted code is not kept: it is pasted again.
            self.server.clear();
            self.code.clear();
        }
        SignInStep::Invite {
            server: self.server.clone(),
            problem,
        }
    }

    fn fresh_code(&mut self) -> SignInStep {
        self.server.clear();
        self.code.clear();
        self.address_first = false;
        SignInStep::Invite {
            server: String::new(),
            problem: None,
        }
    }

    fn take_code(&mut self, raw: &str) -> SignInStep {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return self.bad_code("Paste the code you were given.");
        }
        let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if let Some((origin, token)) = protocol::unpack_join(&compact) {
            return match base_url(&origin) {
                Some(server) => {
                    self.server = server;
                    self.code = token;
                    self.address_first = false;
                    self.begin_browser()
                }
                None => self.bad_code("That code doesn't work. Ask for a new one."),
            };
        }
        if !self.server.is_empty() && bare_token(trimmed) {
            self.code = trimmed.to_owned();
            return self.begin_browser();
        }
        if bare_token(&compact) {
            return self.bad_code("That code has no server in it. Ask for a new one.");
        }
        self.bad_code("That code doesn't work. Ask for a new one.")
    }

    fn bad_code(&self, reason: &str) -> SignInStep {
        SignInStep::Invite {
            server: self.server.clone(),
            problem: Some(SignInProblem::Other { reason: reason.to_owned() }),
        }
    }

    fn browser_step(&self, waiting: bool, problem: Option<SignInProblem>) -> SignInStep {
        SignInStep::Browser {
            server: self.server.clone(),
            provider: self.provider.clone(),
            waiting,
            problem,
        }
    }

    /// Stops listening for the browser, if it was.
    fn stop(&self) {
        if let Some(out) = &self.out {
            out.stop.store(true, Ordering::Relaxed);
            // A phone's authentication session goes with it.
            browser::close_auth();
        }
    }

    /// Asks the server for the provider and where to send the browser, listens for it, and
    /// opens it. A problem before the browser is out shows on the step that asked.
    fn begin_browser(&mut self) -> SignInStep {
        self.stop();
        self.out = None;
        if let Some(step) = self.plain_http_gate() {
            return step;
        }
        let (stay, failed) = match auth_config(&self.server) {
            Ok(provider) => {
                self.provider = provider;
                (None, None)
            }
            Err(fault) => outcome(fault),
        };
        if let Some(reason) = failed {
            return SignInStep::Failed { reason };
        }
        if let Some(problem) = stay {
            return self.stay(problem);
        }
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(_) => return SignInStep::Failed { reason: NO_LISTEN.to_owned() },
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(_) => return SignInStep::Failed { reason: NO_LISTEN.to_owned() },
        };
        let (verifier, state) = match (browser::random(), browser::random()) {
            (Ok(verifier), Ok(state)) => (verifier, state),
            (Err(reason), _) | (_, Err(reason)) => return SignInStep::Failed { reason },
        };
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        // A phone's authentication session is told where the page comes back to.
        browser::set_callback(&redirect_uri);
        let invite = (!self.code.is_empty()).then_some(self.code.as_str());
        let authorize_url = match auth_start(&self.server, &redirect_uri, &browser::challenge(&verifier), &state, invite) {
            Ok(url) if browser::web_link(&url) => url,
            Ok(_) => return SignInStep::Failed { reason: BAD_PAGE.to_owned() },
            Err(fault) => {
                return match outcome(fault) {
                    (_, Some(reason)) => SignInStep::Failed { reason },
                    (problem, None) => self.stay(problem.unwrap_or(SignInProblem::Unreachable)),
                }
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let wait = Wait {
            hub: self.hub.clone(),
            server: self.server.clone(),
            provider: self.provider.clone(),
            state,
            verifier,
            patience: self.browser.patience,
            stop: stop.clone(),
            adopt: self.adopt.clone(),
            plain_http_ok: self.plain_http_ok,
        };
        let spawned = std::thread::Builder::new()
            .name("tiny-signin".into())
            .spawn(move || wait.run(listener));
        if spawned.is_err() {
            return SignInStep::Failed { reason: NO_LISTEN.to_owned() };
        }
        self.out = Some(Out { authorize_url, stop });
        self.open_again()
    }

    /// A server reached over plain http that is not on this computer or a network of the person's
    /// own gets nothing sent to it until the person has been told in words and asks for the same
    /// server again: nothing is asked of it, not even its sign-in page. The yes is kept
    /// (`plain_http_ok`) and goes into the session. An address on this computer or the person's
    /// own network is not stopped; the sheet shows it a warning of its own.
    fn plain_http_gate(&mut self) -> Option<SignInStep> {
        self.plain_http_ok = false;
        if protocol::reach(&self.server) != protocol::Reach::Open {
            return None;
        }
        if self.warned == self.server {
            self.plain_http_ok = true;
            return None;
        }
        self.warned = self.server.clone();
        let words = protocol::plain_http_words(protocol::Reach::Open).unwrap_or_default();
        Some(self.stay(SignInProblem::Other { reason: format!("{words} {CONFIRM_PLAIN_HTTP}") }))
    }

    /// A problem before the browser is out. The first try stays on the step that asked; a try
    /// again stays on the browser's.
    fn stay(&mut self, problem: SignInProblem) -> SignInStep {
        if matches!(self.step, SignInStep::Browser { .. }) {
            self.browser_step(false, Some(problem))
        } else {
            self.asking(Some(problem))
        }
    }

    /// The page the browser was sent to, once more. It is the same sign-in.
    fn open_again(&self) -> SignInStep {
        self.browser_step(self.out.is_some(), None)
    }
}

/// A failure, as the sheet shows it. `.1` ends the sheet: the answer could not be read.
fn outcome(fault: Fault) -> (Option<SignInProblem>, Option<String>) {
    let problem = match fault {
        Fault::SlowDown => SignInProblem::TooMany {
            until: unix_now() + 60,
        },
        Fault::Unavailable => SignInProblem::Other {
            reason: "The server had a problem. Try again in a moment.".to_owned(),
        },
        Fault::Unreachable | Fault::Unauthorized => SignInProblem::Unreachable,
        Fault::BadAnswer => return (None, Some("The server's answer could not be read.".to_owned())),
        Fault::Words(reason) => SignInProblem::Other {
            reason: reason.to_owned(),
        },
    };
    (Some(problem), None)
}

/// One wait for the browser, on a thread of its own. It owns the listener.
struct Wait {
    hub: Arc<Hub>,
    server: String,
    provider: String,
    state: String,
    verifier: String,
    patience: Duration,
    stop: Arc<AtomicBool>,
    adopt: Arc<Adopt>,
    plain_http_ok: bool,
}

impl Wait {
    fn run(self, listener: TcpListener) {
        let back = browser::wait(listener, &self.state, self.patience, &self.stop);
        // The page came back, or it never will: a phone takes its authentication session away.
        browser::close_auth();
        let stopped = |problem| SignInStep::Browser {
            server: self.server.clone(),
            provider: self.provider.clone(),
            waiting: false,
            problem: Some(problem),
        };
        let next = match back {
            Back::Stopped => return,
            Back::TimedOut => stopped(SignInProblem::Other { reason: TIMED_OUT.to_owned() }),
            Back::Error(error) => stopped(match error.as_str() {
                "not_member" => SignInProblem::NotMember,
                "denied" => SignInProblem::Other { reason: DENIED.to_owned() },
                _ => SignInProblem::Other { reason: FAILED.to_owned() },
            }),
            Back::Code(_) if self.stop.load(Ordering::Relaxed) => return,
            Back::Code(code) => match auth_token(&self.server, &code, &self.verifier) {
                Ok(_) if self.stop.load(Ordering::Relaxed) => return,
                Ok(saved) => self.kept(saved),
                Err(fault) => match outcome(fault) {
                    (_, Some(reason)) => SignInStep::Failed { reason },
                    (problem, None) => SignInStep::Browser {
                        server: self.server.clone(),
                        provider: self.provider.clone(),
                        waiting: false,
                        problem,
                    },
                },
            },
        };
        self.show(next);
    }

    fn kept(&self, mut saved: super::Saved) -> SignInStep {
        saved.plain_http_ok = self.plain_http_ok;
        let account = saved.user_id.clone();
        let Ok(session) = serde_json::to_string(&saved) else {
            return SignInStep::Failed {
                reason: "The server's answer could not be kept.".to_owned(),
            };
        };
        match (self.adopt)(&session) {
            Ok(_) => SignInStep::Done {
                account: Some(AccountKey {
                    network: Network::Tiny,
                    account,
                }),
            },
            Err(_) => SignInStep::Failed {
                reason: "The sign-in could not be kept.".to_owned(),
            },
        }
    }

    /// Tells the sheet, when this is still the sign-in on show.
    fn show(&self, step: SignInStep) {
        let mut held = current();
        let Some(flow) = held.as_mut() else {
            return;
        };
        if !flow.out.as_ref().is_some_and(|out| Arc::ptr_eq(&out.stop, &self.stop)) || self.stop.load(Ordering::Relaxed) {
            return;
        }
        flow.out = None;
        if matches!(step, SignInStep::Done { .. } | SignInStep::Failed { .. }) {
            *held = None;
        } else {
            flow.step = step.clone();
        }
        drop(held);
        self.hub.changed(Change::SignIn {
            network: Network::Tiny,
            step,
        });
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{http, serve};
    use super::*;
    use crate::store::Store;
    use std::io::{Read, Write};
    use std::time::Instant;

    fn hub() -> Arc<Hub> {
        Arc::new(Hub::new(
            Store::open(std::path::Path::new(":memory:")).expect("a store in memory"),
        ))
    }

    fn adopt() -> Adopt {
        Box::new(|_session| Ok(Vec::new()))
    }

    fn one_sheet() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    const ADA: &str = "22222222-2222-2222-2222-222222222222";

    /// What the fake server heard, and what its browser was sent to.
    #[derive(Default)]
    struct Heard {
        requests: Vec<String>,
        /// From each `POST /v1/auth/start`: the redirect and the client's state.
        starts: Vec<(String, String)>,
        opened: Vec<String>,
    }

    fn body(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap_or(serde_json::Value::Null)
    }

    /// A tinyserver that speaks the `/v1/auth/*` routes of the contract. Its token route answers
    /// only the code it handed out, with the verifier whose challenge came to `start`.
    fn server(heard: Arc<Mutex<Heard>>) -> String {
        let challenge = Arc::new(Mutex::new(String::new()));
        serve(move |request| {
            let mut heard = heard.lock().unwrap();
            heard.requests.push(request.to_owned());
            let first = request.lines().next().unwrap_or("");
            let sent = body(request);
            if first.starts_with("GET /v1/auth/config ") {
                return http(200, "OK", r#"{"methods":["oidc"],"provider_name":"Tinyauth"}"#);
            }
            if first.starts_with("POST /v1/auth/start ") {
                let redirect = sent["redirect_uri"].as_str().unwrap_or("").to_owned();
                let state = sent["state"].as_str().unwrap_or("").to_owned();
                *challenge.lock().unwrap() = sent["code_challenge"].as_str().unwrap_or("").to_owned();
                heard.starts.push((redirect, state));
                return http(200, "OK", r#"{"authorize_url":"https://auth.example/authorize?client_id=tinyserver&state=server-state"}"#);
            }
            if first.starts_with("POST /v1/auth/token ") {
                let verifier = sent["code_verifier"].as_str().unwrap_or("");
                if sent["code"] != "one-time-code" || browser::challenge(verifier) != *challenge.lock().unwrap() {
                    return http(400, "Bad Request", r#"{"error":"invalid_grant"}"#);
                }
                return http(
                    200,
                    "OK",
                    &format!(r#"{{"token":"tok_join","user_id":"{ADA}","email":null,"display_name":"Ada","role":"member"}}"#),
                );
            }
            http(404, "Not Found", r#"{"error":"not_found"}"#)
        })
    }

    /// The browser: it goes to the provider, and the provider sends it back with `query`, which
    /// may name `{state}`. Only the first time it is opened; after that it stays on the page.
    /// What the loopback answered is kept.
    fn browser_that(heard: Arc<Mutex<Heard>>, query: &'static str, pages: Arc<Mutex<Vec<String>>>) -> Browser {
        Browser {
            open: Arc::new(move |url: &str| {
                let mut seen = heard.lock().unwrap();
                seen.opened.push(url.to_owned());
                if seen.opened.len() > 1 {
                    return Ok(());
                }
                let (redirect, state) = seen.starts.last().cloned().unwrap_or_default();
                let pages = pages.clone();
                std::thread::spawn(move || {
                    let page = get(&redirect, &query.replace("{state}", &state));
                    pages.lock().unwrap().push(page);
                });
                Ok(())
            }),
            patience: Duration::from_secs(5),
        }
    }

    /// The browser's one request to the loopback, and what it answered.
    fn get(redirect: &str, query: &str) -> String {
        let rest = redirect.strip_prefix("http://").unwrap_or(redirect);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let Ok(mut stream) = std::net::TcpStream::connect(host) else {
            return String::new();
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = write!(stream, "GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    }

    /// What the loopback answered the browser. The browser may still be reading it when the
    /// sign-in has already moved on.
    fn first_page(pages: &Mutex<Vec<String>>) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(page) = pages.lock().unwrap().first() {
                return page.clone();
            }
            assert!(Instant::now() < deadline, "the browser got no page");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The next step the sign-in shows by itself.
    fn next_step(hub: &Hub, since: &mut u64) -> SignInStep {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let (seq, _, changes) = hub.wait(*since, Duration::from_millis(200));
            *since = seq;
            for change in changes {
                if let Change::SignIn { network: Network::Tiny, step } = change {
                    return step;
                }
            }
        }
        panic!("the sign-in did not move");
    }

    #[test]
    fn a_slow_browser_does_not_block_cancellation() {
        let _one = one_sheet();
        cancel();
        let hub = hub();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let base = server(heard);
        let packed = protocol::pack_join(&base, "invite").unwrap();
        let (entered, entering) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        step_with(&hub, None, adopt(), || Browser {
            open: Arc::new(move |_| {
                entered.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                Ok(())
            }),
            patience: Duration::from_secs(5),
        }).unwrap();
        let worker = std::thread::spawn(move || {
            step(&hub, Some(SignInInput::Invite { code: packed }), adopt()).unwrap()
        });
        entering.recv_timeout(Duration::from_secs(5)).unwrap();
        // try_lock makes a regression fail immediately instead of deadlocking the test.
        assert!(FLOW.try_lock().is_ok(), "browser launch held the sign-in lock");
        cancel();
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(current().is_none(), "a stale launcher restored a cancelled flow");
    }

    #[test]
    fn a_bad_address_stays_on_the_server_step() {
        let _one = one_sheet();
        cancel();
        let hub = hub();
        assert_eq!(
            step(&hub, None, adopt()).unwrap(),
            SignInStep::Invite { server: String::new(), problem: None }
        );
        assert_eq!(
            step(&hub, Some(SignInInput::HaveCode { url: String::new() }), adopt()).unwrap(),
            SignInStep::Server { problem: None }
        );
        for bad in ["not a url", "http://user:pw@127.0.0.1:8080", "ftp://127.0.0.1"] {
            assert_eq!(
                step(&hub, Some(SignInInput::Server { url: bad.into() }), adopt()).unwrap(),
                SignInStep::Server { problem: Some(SignInProblem::UrlInvalid) }
            );
        }
        // A server that cannot be reached stays on the address, saying so.
        assert_eq!(
            step(&hub, Some(SignInInput::Server { url: "http://127.0.0.1:9".into() }), adopt()).unwrap(),
            SignInStep::Server { problem: Some(SignInProblem::Unreachable) }
        );
        assert_eq!(
            step(&hub, Some(SignInInput::Back), adopt()).unwrap(),
            SignInStep::Invite { server: String::new(), problem: None }
        );
        cancel();
    }


    #[test]
    fn plain_http_off_the_persons_network_is_asked_about_before_anything_is_sent() {
        let _one = one_sheet();
        cancel();
        let hub = hub();
        step(&hub, None, adopt()).unwrap();
        step(&hub, Some(SignInInput::HaveCode { url: String::new() }), adopt()).unwrap();
        let typed = |hub: &Arc<Hub>, url: &str| step(hub, Some(SignInInput::Server { url: url.into() }), adopt()).unwrap();
        let words = |step: SignInStep| match step {
            SignInStep::Server { problem: Some(SignInProblem::Other { reason }) } => reason,
            other => panic!("not the question: {other:?}"),
        };
        // First try: a question in words, on the address that was typed. `.invalid` never
        // resolves, so had anything been sent the answer would be "could not reach".
        let asked = words(typed(&hub, "http://chat.example.invalid"));
        assert!(asked.contains("http, not https") && asked.contains("not on your own network"), "{asked}");
        assert!(asked.contains("continue again"), "it says how to go on: {asked}");
        // A different address starts the question over.
        assert!(words(typed(&hub, "http://other.example.invalid")).contains("http, not https"));
        // The same one again is a yes: now it is asked for its sign-in page, which fails here.
        assert_eq!(
            typed(&hub, "http://other.example.invalid"),
            SignInStep::Server { problem: Some(SignInProblem::Unreachable) },
            "past the question the sign-in goes ahead"
        );
        // This computer is not stopped. (A person's own network is not either: see `protocol::reach`.)
        let reply = typed(&hub, "http://127.0.0.1:9");
        assert_eq!(reply, SignInStep::Server { problem: Some(SignInProblem::Unreachable) });
        // https is never asked about.
        assert_eq!(
            typed(&hub, "https://chat.example.invalid"),
            SignInStep::Server { problem: Some(SignInProblem::Unreachable) }
        );
        cancel();
    }

    #[test]
    fn a_yes_to_plain_http_goes_into_the_session_and_nothing_else_does() {
        let kept = Arc::new(Mutex::new(Vec::<String>::new()));
        let wait = |ok: bool| {
            let kept = kept.clone();
            let adopt: Adopt = Box::new(move |session: &str| {
                kept.lock().unwrap().push(session.to_owned());
                Ok(Vec::new())
            });
            Wait {
                hub: hub(),
                server: "http://chat.example.invalid".into(),
                provider: String::new(),
                state: String::new(),
                verifier: String::new(),
                patience: Duration::from_secs(1),
                stop: Arc::new(AtomicBool::new(false)),
                adopt: Arc::new(adopt),
                plain_http_ok: ok,
            }
        };
        let session = || super::super::saved_from("http://chat.example.invalid", serde_json::json!({ "token": "t", "user_id": ADA, "email": null, "display_name": "Ada", "role": "member" })).ok().expect("a session");
        for ok in [false, true] {
            // A fresh answer from the server never carries the flag itself.
            assert!(!session().plain_http_ok);
            wait(ok).kept(session());
        }
        let kept = kept.lock().unwrap();
        assert!(!kept[0].contains("plain_http_ok"), "no yes, no field: {}", kept[0]);
        assert!(kept[1].contains("\"plain_http_ok\":true"), "{}", kept[1]);
        assert!(super::super::parse(&kept[1]).is_ok());
    }
    #[test]
    fn the_address_is_an_origin() {
        assert_eq!(base_url("  https://Example.com/  ").as_deref(), Some("https://Example.com"));
        assert_eq!(base_url("private.tinyclient.io").as_deref(), Some("https://private.tinyclient.io"));
        assert_eq!(base_url("127.0.0.1:8080").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(base_url("localhost").as_deref(), Some("http://localhost"));
        assert_eq!(base_url("http://[::1]:8080").as_deref(), Some("http://[::1]:8080"));
        assert_eq!(base_url("http://127.0.0.1:8080?x=1").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(base_url("http://127.0.0.1:8080#top").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(base_url("https://chat.example/rooms?x=1#y").as_deref(), Some("https://chat.example"));
        assert_eq!(base_url("chat.example/health").as_deref(), Some("https://chat.example"));
        assert_eq!(base_url("localhost:8080/health").as_deref(), Some("http://localhost:8080"));
        assert_eq!(base_url("ftp://127.0.0.1:8080"), None);
        assert_eq!(base_url("http://127.0.0.1:0"), None);
        assert!(!format!("{:?}", SignInInput::Server { url: "http://secret.example".into() }).contains("secret"));
        assert!(!format!("{:?}", SignInInput::Invite { code: "secret-code".into() }).contains("secret"));
    }

    /// The whole join: a pasted code, the browser out and back, and the session kept as before.
    /// A phone's authentication session (`protocol::web_sign_in`), as a fake: it catches the
    /// provider's redirect and hands it over instead of loading it, and records being closed.
    struct Phone {
        heard: Arc<Mutex<Heard>>,
        closed: Arc<std::sync::atomic::AtomicBool>,
        callbacks: Mutex<Vec<String>>,
    }

    struct Session(Arc<std::sync::atomic::AtomicBool>);

    impl protocol::web_sign_in::AuthHandle for Session {
        fn closed(&mut self) -> bool {
            false
        }
        fn close(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    impl protocol::web_sign_in::WebSignIn for Phone {
        fn open_page(&self, _: protocol::web_sign_in::WebPage) -> Result<Box<dyn protocol::web_sign_in::WebPageHandle>, String> {
            Err("a Tiny sign-in opens no in-app page".to_owned())
        }
        fn open_auth(&self, auth: protocol::web_sign_in::AuthRequest) -> Result<Box<dyn protocol::web_sign_in::AuthHandle>, String> {
            let mut heard = self.heard.lock().unwrap();
            heard.opened.push(auth.url.clone());
            let (redirect, state) = heard.starts.last().cloned().unwrap_or_default();
            assert_eq!(auth.callback, redirect, "the session is told the loopback redirect");
            self.callbacks.lock().unwrap().push(auth.callback.clone());
            // Someone else's redirect first: it is not delivered.
            (auth.redirected)("http://127.0.0.1:1/callback?code=stolen&state=x".to_owned());
            (auth.redirected)(format!("{redirect}?code=one-time-code&state={state}"));
            Ok(Box::new(Session(self.closed.clone())))
        }
    }

    #[test]
    fn a_phone_signs_in_through_its_authentication_session_and_it_is_closed_after() {
        let _one = one_sheet();
        cancel();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let base = server(heard.clone());
        let packed = protocol::pack_join(&base, "invite").unwrap();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let phone = Arc::new(Phone { heard: heard.clone(), closed: closed.clone(), callbacks: Mutex::default() });
        protocol::web_sign_in::register(phone.clone());
        let kept = Arc::new(Mutex::new(None::<String>));
        let keep = kept.clone();
        let hub = hub();
        let mut since = hub.seq();
        // The system opener, as a phone runs it: no browser, the platform's session.
        step_with(&hub, None, Box::new(move |session| {
            *keep.lock().unwrap() = Some(session.to_owned());
            Ok(Vec::new())
        }), Browser::system).unwrap();
        let shown = step(&hub, Some(SignInInput::Invite { code: packed }), adopt()).unwrap();
        assert!(matches!(shown, SignInStep::Browser { waiting: true, problem: None, .. }), "{shown:?}");
        let done = next_step(&hub, &mut since);
        protocol::web_sign_in::unregister();
        assert_eq!(done, SignInStep::Done { account: Some(AccountKey { network: Network::Tiny, account: ADA.into() }) });
        assert!(kept.lock().unwrap().as_deref().is_some_and(|session| session.contains("tok_join")));
        assert_eq!(heard.lock().unwrap().opened, ["https://auth.example/authorize?client_id=tinyserver&state=server-state"]);
        assert!(phone.callbacks.lock().unwrap()[0].starts_with("http://127.0.0.1:"));
        assert!(closed.load(Ordering::Relaxed), "the authentication session is taken away once the page came back");
    }

    #[test]
    fn a_code_signs_in_through_the_browser_and_the_server_gets_only_the_token() {
        let _one = one_sheet();
        cancel();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let base = server(heard.clone());
        let token = "abcdefghijklmnopqrstuvwxyz0123456789-_ABC";
        let packed = protocol::pack_join(&base, token).unwrap();
        let kept = Arc::new(Mutex::new(None::<String>));
        let keep = kept.clone();
        let pages = Arc::new(Mutex::new(Vec::new()));
        let opens = browser_that(heard.clone(), "code=one-time-code&state={state}", pages.clone());
        let hub = hub();
        let mut since = hub.seq();
        let begun = step_with(
            &hub,
            None,
            Box::new(move |session| {
                *keep.lock().unwrap() = Some(session.to_owned());
                Ok(Vec::new())
            }),
            || opens,
        );
        assert_eq!(begun.unwrap(), SignInStep::Invite { server: String::new(), problem: None });
        // A bare token has no server in it, and nothing is asked of any.
        assert_eq!(
            step(&hub, Some(SignInInput::Invite { code: token.to_owned() }), adopt()).unwrap(),
            SignInStep::Invite {
                server: String::new(),
                problem: Some(SignInProblem::Other { reason: "That code has no server in it. Ask for a new one.".into() })
            }
        );
        assert!(heard.lock().unwrap().requests.is_empty());
        assert_eq!(
            step(&hub, Some(SignInInput::Invite { code: format!(" {packed}\n") }), adopt()).unwrap(),
            SignInStep::Browser { server: base.clone(), provider: "Tinyauth".into(), waiting: true, problem: None }
        );
        assert_eq!(
            next_step(&hub, &mut since),
            SignInStep::Done { account: Some(AccountKey { network: Network::Tiny, account: ADA.into() }) }
        );

        let heard = heard.lock().unwrap();
        assert_eq!(heard.opened, ["https://auth.example/authorize?client_id=tinyserver&state=server-state"]);
        let firsts: Vec<&str> = heard.requests.iter().map(|request| request.lines().next().unwrap_or("")).collect();
        assert!(firsts[0].starts_with("GET /v1/auth/config "));
        assert!(firsts[1].starts_with("POST /v1/auth/start "));
        assert!(firsts[2].starts_with("POST /v1/auth/token "));
        assert_eq!(firsts.len(), 3);
        let start = body(&heard.requests[1]);
        assert_eq!(start["invite"], token, "the bare token, never the pasted code");
        assert_eq!(start["code_challenge_method"], "S256");
        let redirect = start["redirect_uri"].as_str().unwrap();
        let port = redirect.strip_prefix("http://127.0.0.1:").and_then(|rest| rest.strip_suffix("/callback")).unwrap();
        assert!(port.parse::<u16>().is_ok_and(|port| port > 0), "{redirect}");
        let state = start["state"].as_str().unwrap();
        assert!((16..=128).contains(&state.len()));
        let exchanged = body(&heard.requests[2]);
        assert_eq!(exchanged["code"], "one-time-code");
        let verifier = exchanged["code_verifier"].as_str().unwrap();
        assert_eq!(verifier.len(), 43);
        assert_eq!(browser::challenge(verifier), start["code_challenge"]);
        assert!(!heard.requests[1].contains(verifier), "the verifier stays here until the code is back");

        let page = first_page(&pages);
        assert!(page.starts_with("HTTP/1.1 200 "), "{page}");
        assert!(page.contains("You can close this tab and return to tinyclient."));
        let saved: serde_json::Value = serde_json::from_str(&kept.lock().unwrap().clone().expect("adopted")).unwrap();
        assert_eq!(saved["base_url"], base);
        assert_eq!(saved["token"], "tok_join");
        assert_eq!(saved["user_id"], ADA);
        assert_eq!(saved["email"], serde_json::Value::Null);
        assert_eq!(saved["display_name"], "Ada");
        assert_eq!(saved["role"], "member");
        // Over: the next answer starts nothing.
        assert_eq!(step(&hub, Some(SignInInput::OpenBrowser), adopt()).unwrap(), SignInStep::Failed { reason: OVER.into() });
        cancel();
    }

    /// An account with no code sends no invite. A provider account that is not a member is told
    /// so, and trying again begins a new sign-in that a page from another one cannot finish.
    #[test]
    fn an_account_with_no_code_is_told_when_it_is_not_a_member_and_can_try_again() {
        let _one = one_sheet();
        cancel();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let base = server(heard.clone());
        let pages = Arc::new(Mutex::new(Vec::new()));
        let not_member = browser_that(heard.clone(), "error=not_member&state={state}", pages.clone());
        let hub = hub();
        let mut since = hub.seq();
        step_with(&hub, None, adopt(), || not_member).unwrap();
        step(&hub, Some(SignInInput::HaveCode { url: String::new() }), adopt()).unwrap();
        assert_eq!(
            step(&hub, Some(SignInInput::Server { url: base.clone() }), adopt()).unwrap(),
            SignInStep::Browser { server: base.clone(), provider: "Tinyauth".into(), waiting: true, problem: None }
        );
        let told = SignInStep::Browser { server: base.clone(), provider: "Tinyauth".into(), waiting: false, problem: Some(SignInProblem::NotMember) };
        assert_eq!(next_step(&hub, &mut since), told);
        assert_eq!(step(&hub, None, adopt()).unwrap(), told, "the sheet opened again reads where it stands");
        assert!(first_page(&pages).contains("You&#39;re not a member of this server yet."));
        assert!(body(&heard.lock().unwrap().requests[1]).get("invite").is_none());

        // Again: a page with the first sign-in's state is turned away and does not end this one.
        let first_state = heard.lock().unwrap().starts[0].1.clone();
        let again = step(&hub, Some(SignInInput::OpenBrowser), adopt()).unwrap();
        assert!(matches!(again, SignInStep::Browser { waiting: true, problem: None, .. }), "{again:?}");
        let (redirect, state) = heard.lock().unwrap().starts[1].clone();
        assert_ne!(state, first_state);
        let stale = get(&redirect, &format!("code=one-time-code&state={first_state}"));
        assert!(stale.starts_with("HTTP/1.1 400 "), "{stale}");
        let favicon = get(&redirect.replace("/callback", "/favicon.ico"), "");
        assert!(favicon.starts_with("HTTP/1.1 404 "), "{favicon}");
        assert!(get(&redirect, &format!("code=one-time-code&state={state}")).contains("You can close this tab"));
        assert_eq!(
            next_step(&hub, &mut since),
            SignInStep::Done { account: Some(AccountKey { network: Network::Tiny, account: ADA.into() }) }
        );
        cancel();
    }

    /// Open the page again while waiting shows the same page. Closing the sheet closes the
    /// listener, and a browser that never comes back ends the wait.
    #[test]
    fn cancel_closes_the_listener_and_a_browser_that_never_returns_runs_out() {
        let _one = one_sheet();
        cancel();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let base = server(heard.clone());
        let opened = heard.clone();
        let quiet = move |patience| Browser {
            open: Arc::new({
                let opened = opened.clone();
                move |url: &str| {
                    opened.lock().unwrap().opened.push(url.to_owned());
                    Ok(())
                }
            }),
            patience,
        };
        let hub = hub();
        let wait = quiet(Duration::from_secs(60));
        step_with(&hub, None, adopt(), || wait).unwrap();
        step(&hub, Some(SignInInput::HaveCode { url: String::new() }), adopt()).unwrap();
        step(&hub, Some(SignInInput::Server { url: base.clone() }), adopt()).unwrap();
        let again = step(&hub, Some(SignInInput::OpenBrowser), adopt()).unwrap();
        assert!(matches!(again, SignInStep::Browser { waiting: true, .. }));
        assert_eq!(heard.lock().unwrap().opened.len(), 2);
        assert_eq!(heard.lock().unwrap().starts.len(), 1, "the same sign-in, not a new one");
        let (redirect, _) = heard.lock().unwrap().starts[0].clone();
        let host = redirect.strip_prefix("http://").and_then(|rest| rest.strip_suffix("/callback")).unwrap().to_owned();
        assert!(std::net::TcpStream::connect(&host).is_ok(), "listening while it waits");
        cancel();
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::net::TcpStream::connect(&host).is_ok() {
            assert!(Instant::now() < deadline, "the listener is still open after cancel");
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut since = hub.seq();
        let short = quiet(Duration::from_millis(300));
        step_with(&hub, None, adopt(), || short).unwrap();
        step(&hub, Some(SignInInput::HaveCode { url: String::new() }), adopt()).unwrap();
        step(&hub, Some(SignInInput::Server { url: base.clone() }), adopt()).unwrap();
        assert_eq!(
            next_step(&hub, &mut since),
            SignInStep::Browser {
                server: base.clone(),
                provider: "Tinyauth".into(),
                waiting: false,
                problem: Some(SignInProblem::Other { reason: TIMED_OUT.into() })
            }
        );
        cancel();
    }

    #[test]
    fn a_server_from_before_the_browser_sign_in_says_so_on_the_code() {
        let _one = one_sheet();
        cancel();
        let older = serve(|_| http(404, "Not Found", r#"{"error":"not_found"}"#));
        let hub = hub();
        step(&hub, None, adopt()).unwrap();
        let packed = protocol::pack_join(&older, "join-code").unwrap();
        assert_eq!(
            step(&hub, Some(SignInInput::Invite { code: packed }), adopt()).unwrap(),
            SignInStep::Invite { server: String::new(), problem: Some(SignInProblem::Other { reason: super::super::NO_BROWSER.into() }) }
        );
        cancel();
    }
}
