//! Discord's HTTP API: one identity on every request, each route's rate limit honored, and a
//! refusal turned into words the user can read.

use super::super::net::{address, agent, browser};
use super::super::Context;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The web client speaks v9.
const API: &str = "https://discord.com/api/v9";
/// The page that serves the web client, and with it the build it is on.
const LOGIN: &str = "https://discord.com/login";
/// The web client's build, sent when the current one cannot be read and none is remembered.
/// Discord weighs it, and a stale one is itself a signal, so `build` asks first. Read from
/// discord.com on 2026-10-13.
const BUILD: u64 = 636_431;
/// How long the page that names the build may take. Past this the remembered one is used.
const BUILD_PATIENCE: Duration = Duration::from_secs(5);

/// The build the web client is on right now, from the page that serves it. `None` when the
/// page did not answer in time or does not say.
///
/// A fake server in a test is asked only when the test names the page too
/// (`TINYCLIENT_DISCORD_LOGIN`).
fn build(user_agent: &str) -> Option<u64> {
    let faked = std::env::var_os("TINYCLIENT_DISCORD_API").is_some();
    if faked && std::env::var_os("TINYCLIENT_DISCORD_LOGIN").is_none() {
        return None;
    }
    let agent: ureq::Agent = crate::adapters::net::apply_proxy(ureq::Agent::config_builder())
        .tls_config(crate::tls::config())
        .timeout_global(Some(BUILD_PATIENCE))
        .http_status_as_error(false)
        .user_agent(user_agent)
        .build()
        .into();
    let mut response = agent.get(address("TINYCLIENT_DISCORD_LOGIN", LOGIN)).call().ok()?;
    let page = response.body_mut().read_to_string().ok()?;
    let rest = page.split_once("\"BUILD_NUMBER\":\"")?.1;
    rest.split_once('"')?.0.parse::<u64>().ok()
}

/// A wait longer than this is the user's to decide, not ours to sit through.
const PATIENCE: f64 = 30.;
/// Interactive message sends cap 429 retry waits here so sends fail fast instead of stalling.
pub(super) const SEND_PATIENCE: f64 = 8.;
/// Short timeout for best-effort calls like typing indicators.
const BEST_EFFORT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum route -> bucket mappings kept in memory.
const BUCKETS_LIMIT: usize = 512;
/// Discord bans an address for a day after 10,000 answers of 401, 403 or 429 in ten minutes.
/// This many in that time and nothing more is asked until the ten minutes are over.
const REFUSALS: usize = 50;
const REFUSALS_WITHIN: Duration = Duration::from_secs(600);

/// The zone's name, as a browser reports it (IANA id).
fn timezone() -> Option<String> {
    iana_time_zone::get_timezone().ok()
}

/// Who the session says it is: the same browser the sign-in window used on this OS.
fn client_fields(user_agent: &str) -> (&'static str, &'static str, &'static str, String) {
    #[cfg(target_os = "macos")]
    {
        let version = user_agent.split_once("Version/").map_or("26.0", |(_, rest)| rest.split(' ').next().unwrap_or("26.0")).to_owned();
        ("Mac OS X", "Safari", "10.15.7", version)
    }
    #[cfg(target_os = "windows")]
    {
        let version = user_agent.split_once("Chrome/").map_or("131.0.0.0", |(_, rest)| rest.split(' ').next().unwrap_or("131.0.0.0")).to_owned();
        ("Windows", "Chrome", "10", version)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let version = user_agent.split_once("Chrome/").map_or("131.0.0.0", |(_, rest)| rest.split(' ').next().unwrap_or("131.0.0.0")).to_owned();
        ("Linux", "Chrome", "", version)
    }
}

/// Who the session says it is: the Discord web client in the browser this OS signs in with.
pub struct Identity {
    pub user_agent: String,
    /// The `properties` of the gateway's identify.
    pub properties: Value,
    /// The same, as the `X-Super-Properties` header.
    pub super_properties: String,
    timezone: Option<String>,
}

impl Identity {
    /// The Safari the sign-in window presented (`net::browser`), so the session keeps one
    /// identity from sign-in through every request and the socket.
    pub fn new(build: u64) -> Identity {
        let user_agent = browser().to_owned();
        let (os, browser_name, os_version, version) = client_fields(&user_agent);
        let properties = json!({
            "os": os,
            "browser": browser_name,
            "device": "",
            "system_locale": "en-US",
            "has_client_mods": false,
            "browser_user_agent": user_agent,
            "browser_version": version,
            "os_version": os_version,
            "referrer": "",
            "referring_domain": "",
            "referrer_current": "",
            "referring_domain_current": "",
            "release_channel": "stable",
            "client_build_number": build,
            "client_event_source": null,
        });
        let super_properties = base64::engine::general_purpose::STANDARD.encode(properties.to_string());
        Identity {
            user_agent,
            properties,
            super_properties,
            timezone: timezone(),
        }
    }
}

/// Why a request did not do what was asked.
#[derive(Debug)]
pub enum Refused {
    /// 401: the session is over.
    Expired,
    /// Discord wants a captcha or another check only its own app can do.
    Challenge,
    /// Still rate limited after waiting once; seconds until it may be tried again.
    Limited(f64),
    /// Too many requests were refused: nothing is asked for this many seconds more.
    Paused(f64),
    Status {
        status: u16,
        code: u64,
        message: String,
        retry_after: Option<f64>,
    },
    Network(String),
}

impl Refused {
    /// The reason in words, for a failed send or a refused command.
    pub fn reason(&self) -> String {
        let whole = |seconds: f64| seconds.ceil().max(1.) as u64;
        match self {
            Refused::Expired => "Discord signed you out. Sign in again.".to_owned(),
            Refused::Challenge => "Discord asked for a check tinyclient cannot do. Finish it in Discord, then try again.".to_owned(),
            Refused::Limited(seconds) => format!("Rate limited: try again in {} s.", whole(*seconds)),
            Refused::Paused(seconds) => {
                format!("Discord refused too many requests. tinyclient asks again in {} min.", whole(*seconds / 60.))
            }
            Refused::Status { code: 20016, retry_after, .. } => {
                format!("Slow mode: wait {} s.", whole(retry_after.unwrap_or(1.)))
            }
            Refused::Status { code: 50013 | 50001, .. } => "You do not have permission to do this here.".to_owned(),
            Refused::Status { code: 50007, .. } => "This person does not accept direct messages from you.".to_owned(),
            Refused::Status { code: 40002, .. } => "Discord wants you to verify your account. Finish it in Discord.".to_owned(),
            // 200000 is AutoMod blocking the message. The sentence is fixed: Discord's own text is not shown.
            Refused::Status { code: 200000 | 200001, .. } => "This server's AutoMod blocked your message.".to_owned(),
            Refused::Status { status: 403, message, .. } if message.is_empty() => {
                "You do not have permission to do this here.".to_owned()
            }
            Refused::Status { status, message, .. } if message.is_empty() => format!("Discord refused ({status})."),
            Refused::Status { message, .. } => format!("Discord refused: {message}"),
            Refused::Network(why) => format!("Discord could not be reached: {why}"),
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

/// What a route allows: requests left in its window, and when the window ends.
struct Window {
    remaining: u32,
    resets: Instant,
}

pub struct Rest {
    ctx: Context,
    /// Made on first use, on an adapter thread: naming the browser runs a program, and chatd
    /// must be serving before anything of the kind.
    agent: OnceLock<ureq::Agent>,
    /// Short-timeout agent for best-effort calls such as typing indicators.
    short_agent: OnceLock<ureq::Agent>,
    identity: OnceLock<Identity>,
    token: String,
    windows: Mutex<HashMap<String, Window>>,
    /// Dynamic `route -> X-RateLimit-Bucket` map learned from Discord response headers.
    buckets: Mutex<HashMap<String, String>>,
    /// Set by a 429 that covers every route.
    everything_waits: Mutex<Option<Instant>>,
    /// Discord challenged this account. Every request is refused until the next sign-in or
    /// restart: retrying a challenged action is how a flag becomes a ban.
    pub challenged: AtomicBool,
    /// A request came back 401. The socket thread reads this and ends the session.
    pub expired: AtomicBool,
    /// When each recent 401, 403 or 429 was answered, and until when nothing is asked because
    /// there were too many.
    refusals: Mutex<(VecDeque<Instant>, Option<Instant>)>,
}

/// The route a limit belongs to: the path with message ids taken out, so every message of a
/// channel shares its channel's window.
fn route(method: &str, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let mut route = String::from(method);
    let mut after_messages = false;
    let mut reactions = false;
    for part in path.split('/').filter(|part| !part.is_empty()) {
        route.push('/');
        let numeric = part.bytes().all(|byte| byte.is_ascii_digit());
        // Every reaction to any message of a channel is one route, whatever the emoji.
        route.push_str(if (after_messages && numeric) || reactions { ":id" } else { part });
        after_messages = part == "messages";
        reactions |= part == "reactions";
    }
    route
}

/// What a 429 says about itself, from its body when that is Discord's JSON and from its
/// headers when it is not (Cloudflare answers with a page).
struct Limit {
    wait: f64,
    /// Every route waits, not only this one.
    global: bool,
    /// The limit is the channel's or server's, not ours: Discord does not hold it against us.
    shared: bool,
}

/// A header's seconds. "NaN" and "inf" parse as numbers; neither is a wait, and NaN would reach
/// `Duration::from_secs_f64` through `clamp` and panic.
fn seconds_of(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok().filter(|seconds| seconds.is_finite())
}

fn limit(problem: &Value, header: impl Fn(&str) -> Option<String>) -> Limit {
    let seconds = |name: &str| header(name).as_deref().and_then(seconds_of);
    let stated = problem["retry_after"].as_f64().filter(|seconds| seconds.is_finite()).or_else(|| seconds("retry-after")).or_else(|| seconds("x-ratelimit-reset-after"));
    let scope = header("x-ratelimit-scope").unwrap_or_default();
    let flagged = header("x-ratelimit-global").is_some_and(|value| value == "true");
    // A 429 that is not Discord's own (no JSON, no scope) is the edge refusing the address:
    // nothing else will get through either. Route-scoped Discord 429s (`"global": false` or
    // `x-ratelimit-scope: user`/`shared`) stay on their own route/bucket.
    let unexplained = problem.is_null() && scope.is_empty();
    Limit {
        wait: stated.unwrap_or(if unexplained { 5. } else { 1. }).max(0.),
        global: problem["global"].as_bool() == Some(true) || flagged || scope == "global" || unexplained,
        shared: scope == "shared",
    }
}

impl Rest {
    pub fn new(token: String, ctx: Context) -> Rest {
        Rest {
            ctx,
            agent: OnceLock::new(),
            short_agent: OnceLock::new(),
            identity: OnceLock::new(),
            token,
            windows: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
            everything_waits: Mutex::new(None),
            challenged: AtomicBool::new(false),
            expired: AtomicBool::new(false),
            refusals: Mutex::new((VecDeque::new(), None)),
        }
    }

    fn short_agent(&self) -> &ureq::Agent {
        self.short_agent.get_or_init(|| {
            crate::adapters::net::apply_proxy(ureq::Agent::config_builder())
                .tls_config(crate::tls::config())
                .timeout_global(Some(BEST_EFFORT_TIMEOUT))
                .http_status_as_error(false)
                .user_agent(browser())
                .build()
                .into()
        })
    }

    fn bucket_key(&self, route: &str) -> String {
        let buckets = crate::hub::lock(&self.buckets);
        match buckets.get(route) {
            Some(bucket) => format!("bucket:{bucket}"),
            None => route.to_owned(),
        }
    }

    fn remember_window(&self, route: &str, bucket: Option<&str>, window: Window) {
        let key = if let Some(bucket) = bucket.filter(|b| !b.is_empty()) {
            let mut buckets = crate::hub::lock(&self.buckets);
            if buckets.len() >= BUCKETS_LIMIT && !buckets.contains_key(route) {
                buckets.clear();
            }
            buckets.insert(route.to_owned(), bucket.to_owned());
            format!("bucket:{bucket}")
        } else {
            self.bucket_key(route)
        };
        crate::hub::lock(&self.windows).insert(key, window);
    }

    /// Who the session says it is. The first call reads the web client's build, waiting at most
    /// [`BUILD_PATIENCE`]; the last one read is kept in a note for when that fails. Call it
    /// from an adapter thread only.
    pub fn identity(&self) -> &Identity {
        self.identity.get_or_init(|| {
            let remembered = self.ctx.note("build").and_then(|build| build.parse::<u64>().ok());
            let read = build(browser()).filter(|build| *build >= BUILD);
            if let Some(read) = read.filter(|read| Some(*read) != remembered) {
                self.ctx.set_note("build", &read.to_string());
            }
            Identity::new(read.or(remembered).unwrap_or(BUILD).max(BUILD))
        })
    }

    /// Waits out what is already known about this route's limit. A long wait is refused instead.
    fn pace(&self, route: &str, max_wait: f64) -> Result<(), Refused> {
        let now = Instant::now();
        let bucket = self.bucket_key(route);
        let window = {
            let mut windows = crate::hub::lock(&self.windows);
            if windows.len() > 64 {
                windows.retain(|_, window| window.resets > now);
            }
            let by_bucket = windows.get(&bucket).filter(|window| window.remaining == 0).map(|window| window.resets);
            let by_route = if bucket != route {
                windows.get(route).filter(|window| window.remaining == 0).map(|window| window.resets)
            } else {
                None
            };
            by_bucket.into_iter().chain(by_route).max()
        };
        let everything = *crate::hub::lock(&self.everything_waits);
        let until = window.into_iter().chain(everything).max().filter(|until| *until > now);
        if let Some(until) = until {
            let wait = until - now;
            if wait.as_secs_f64() > max_wait {
                return Err(Refused::Limited(wait.as_secs_f64()));
            }
            if self.ctx.pause(wait) {
                return Err(Refused::Network("stopped".to_owned()));
            }
        }
        Ok(())
    }

    /// Counts an answer of 401, 403 or 429. Too many in ten minutes and every request is
    /// refused here until they are over, with the account marked as needing attention.
    fn refused(&self) {
        let now = Instant::now();
        let mut refusals = crate::hub::lock(&self.refusals);
        let (recent, paused) = &mut *refusals;
        recent.push_back(now);
        while recent.front().is_some_and(|at| now.duration_since(*at) > REFUSALS_WITHIN) {
            recent.pop_front();
        }
        if recent.len() >= REFUSALS && paused.is_none() {
            *paused = Some(now + REFUSALS_WITHIN);
            recent.clear();
            drop(refusals);
            self.ctx.attention("Discord is refusing tinyclient's requests. Nothing more is asked for 10 minutes; check your account in Discord.");
        }
    }

    /// Seconds left of a pause for too many refusals.
    fn paused(&self) -> Option<f64> {
        let mut refusals = crate::hub::lock(&self.refusals);
        let until = refusals.1?;
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            refusals.1 = None;
            drop(refusals);
            if !self.challenged.load(Ordering::Relaxed) {
                self.ctx.attention("");
            }
            return None;
        }
        Some(left.as_secs_f64())
    }

    /// Discord wants something only its own app can do. Nothing more is asked of it.
    pub fn challenge(&self, what: &str) {
        self.challenged.store(true, Ordering::Relaxed);
        self.ctx.attention(what);
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
        short_timeout: bool,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let identity = self.identity();
        let agent = if short_timeout { self.short_agent() } else { self.agent.get_or_init(agent) };
        macro_rules! dressed {
            ($request:expr) => {{
                let mut request = $request
                    .header("Authorization", &self.token)
                    .header("User-Agent", &identity.user_agent)
                    .header("X-Super-Properties", &identity.super_properties)
                    .header("X-Discord-Locale", "en-US")
                    .header("Accept", "*/*")
                    .header("Accept-Language", "en-US,en;q=0.9")
                    .header("Origin", "https://discord.com")
                    .header("Referer", "https://discord.com/channels/@me");
                if let Some(zone) = &identity.timezone {
                    request = request.header("X-Discord-Timezone", zone);
                }
                for (name, value) in extra {
                    request = request.header(*name, *value);
                }
                request
            }};
        }
        match (method, body) {
            ("GET", _) => dressed!(agent.get(url)).call(),
            ("DELETE", Some(body)) => dressed!(agent.delete(url).force_send_body()).send_json(body),
            ("DELETE", None) => dressed!(agent.delete(url)).call(),
            ("PUT", Some(body)) => dressed!(agent.put(url)).send_json(body),
            ("PUT", None) => dressed!(agent.put(url)).send_empty(),
            ("PATCH", Some(body)) => dressed!(agent.patch(url)).send_json(body),
            ("PATCH", None) => dressed!(agent.patch(url)).send_empty(),
            (_, Some(body)) => dressed!(agent.post(url)).send_json(body),
            (_, None) => dressed!(agent.post(url)).send_empty(),
        }
    }

    pub fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, Refused> {
        self.call_with(method, path, body, &[])
    }

    /// Interactive send call: caps 429 retry waits at [`SEND_PATIENCE`] so sends do not stall.
    pub fn call_send(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, Refused> {
        self.call_inner(method, path, body, &[], true, true, SEND_PATIENCE, false)
    }

    /// Best-effort call (e.g. typing indicators): short timeout, capped short retry on 429, no session captcha lock.
    pub fn call_best_effort(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, Refused> {
        self.call_inner(method, path, body, &[], true, false, BEST_EFFORT_TIMEOUT.as_secs_f64(), true)
    }

    /// One call, with headers of its own beside the session's. The answer is the body as text,
    /// so a caller parses it straight into what it needs. A 429 is waited out in full and
    /// tried once more, then reported.
    pub fn call_with(&self, method: &str, path: &str, body: Option<Value>, extra: &[(&str, &str)]) -> Result<String, Refused> {
        self.call_inner(method, path, body, extra, true, true, PATIENCE, false)
    }

    /// One-shot call for anti-abuse sensitive endpoints (friend requests, server invites): never
    /// retries on HTTP 429, and a CAPTCHA fails only this request without locking the rest of the
    /// Discord session.
    pub fn call_once_with(&self, method: &str, path: &str, body: Option<Value>, extra: &[(&str, &str)]) -> Result<String, Refused> {
        self.call_inner(method, path, body, extra, false, false, PATIENCE, false)
    }

    /// One-shot `POST` with a `multipart/form-data` body (e.g. guild sticker uploads).
    /// Never retries on HTTP 429 so rate-limit wait times surface directly to the user.
    pub fn call_multipart_once(&self, path: &str, boundary: &str, body: &[u8]) -> Result<String, Refused> {
        if self.challenged.load(Ordering::Relaxed) {
            return Err(Refused::Challenge);
        }
        if self.expired.load(Ordering::Relaxed) {
            return Err(Refused::Expired);
        }
        if let Some(left) = self.paused() {
            return Err(Refused::Paused(left));
        }
        let url = format!("{}{path}", address("TINYCLIENT_DISCORD_API", API));
        let route = route("POST", path);
        self.pace(&route, PATIENCE)?;
        let identity = self.identity();
        let agent = self.agent.get_or_init(agent);
        let mut request = agent
            .post(&url)
            .header("Authorization", &self.token)
            .header("User-Agent", &identity.user_agent)
            .header("X-Super-Properties", &identity.super_properties)
            .header("X-Discord-Locale", "en-US")
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Origin", "https://discord.com")
            .header("Referer", "https://discord.com/channels/@me")
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
        if let Some(zone) = &identity.timezone {
            request = request.header("X-Discord-Timezone", zone);
        }
        let mut response = request.send(body).map_err(|err| Refused::Network(err.to_string()))?;
        let headers = response.headers().clone();
        let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
        let bucket = header("x-ratelimit-bucket");
        let remaining = header("x-ratelimit-remaining").and_then(|value| value.parse::<u32>().ok());
        let reset = header("x-ratelimit-reset-after").as_deref().and_then(seconds_of);
        if let (Some(remaining), Some(reset)) = (remaining, reset) {
            let window = Window {
                remaining,
                resets: Instant::now() + Duration::from_secs_f64(reset.clamp(0., 3600.)),
            };
            self.remember_window(&route, bucket.as_deref(), window);
        }
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        if (200..300).contains(&status) {
            return Ok(text);
        }
        let problem: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let code = problem["code"].as_u64().unwrap_or(0);
        let retry_after = problem["retry_after"].as_f64();
        if problem.get("captcha_key").is_some() || problem.get("captcha_sitekey").is_some() {
            return Err(Refused::Challenge);
        }
        match status {
            401 => {
                self.expired.store(true, Ordering::Relaxed);
                Err(Refused::Expired)
            }
            429 if code != 20016 => {
                let limit = limit(&problem, header);
                if !limit.shared {
                    self.refused();
                }
                let until = Instant::now() + Duration::from_secs_f64(limit.wait.min(3600.));
                if limit.global {
                    *crate::hub::lock(&self.everything_waits) = Some(until);
                }
                self.remember_window(&route, bucket.as_deref(), Window { remaining: 0, resets: until });
                Err(Refused::Limited(limit.wait))
            }
            _ => {
                if status == 403 {
                    self.refused();
                }
                Err(Refused::Status {
                    status,
                    code,
                    message: problem["message"].as_str().unwrap_or("").to_owned(),
                    retry_after,
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call_inner(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
        retry_429: bool,
        session_captcha: bool,
        max_wait: f64,
        short_timeout: bool,
    ) -> Result<String, Refused> {
        if self.challenged.load(Ordering::Relaxed) {
            return Err(Refused::Challenge);
        }
        if self.expired.load(Ordering::Relaxed) {
            return Err(Refused::Expired);
        }
        if let Some(left) = self.paused() {
            return Err(Refused::Paused(left));
        }
        let url = format!("{}{path}", address("TINYCLIENT_DISCORD_API", API));
        let route = route(method, path);
        let mut waited = false;
        loop {
            self.pace(&route, max_wait)?;
            let mut response =
                self.request(method, &url, body.as_ref(), extra, short_timeout).map_err(|err| Refused::Network(err.to_string()))?;
            let headers = response.headers().clone();
            let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
            let bucket = header("x-ratelimit-bucket");
            let remaining = header("x-ratelimit-remaining").and_then(|value| value.parse::<u32>().ok());
            let reset = header("x-ratelimit-reset-after").as_deref().and_then(seconds_of);
            if let (Some(remaining), Some(reset)) = (remaining, reset) {
                let window = Window {
                    remaining,
                    resets: Instant::now() + Duration::from_secs_f64(reset.clamp(0., 3600.)),
                };
                self.remember_window(&route, bucket.as_deref(), window);
            }
            let status = response.status().as_u16();
            let text = response.body_mut().read_to_string().unwrap_or_default();
            if (200..300).contains(&status) {
                return Ok(text);
            }
            let problem: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let code = problem["code"].as_u64().unwrap_or(0);
            let retry_after = problem["retry_after"].as_f64();
            if problem.get("captcha_key").is_some() || problem.get("captcha_sitekey").is_some() {
                if session_captcha {
                    self.challenge("Discord asked for a captcha. Finish it in Discord, then sign in here again.");
                }
                return Err(Refused::Challenge);
            }
            match status {
                // A 401 that asks for a 2FA code (60003, 60008) is about this request; the session stands.
                401 if !matches!(code, 60003 | 60008) => {
                    self.expired.store(true, Ordering::Relaxed);
                    return Err(Refused::Expired);
                }
                // Slow mode answers 429 too, but waiting it out is the user's choice.
                429 if code != 20016 => {
                    let limit = limit(&problem, header);
                    if !limit.shared {
                        self.refused();
                    }
                    let until = Instant::now() + Duration::from_secs_f64(limit.wait.min(3600.));
                    if limit.global {
                        *crate::hub::lock(&self.everything_waits) = Some(until);
                    }
                    self.remember_window(&route, bucket.as_deref(), Window { remaining: 0, resets: until });
                    if !retry_429 || waited || limit.wait > max_wait || self.paused().is_some() {
                        return Err(Refused::Limited(limit.wait));
                    }
                    waited = true;
                }
                _ => {
                    if status == 403 {
                        self.refused();
                    }
                    return Err(Refused::Status {
                        status,
                        code,
                        message: problem["message"].as_str().unwrap_or("").to_owned(),
                        retry_after,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_of_a_channel_shares_its_channels_window() {
        assert_eq!(route("POST", "/channels/5/messages/77/ack"), route("POST", "/channels/5/messages/78/ack"));
        assert_ne!(route("GET", "/channels/5/messages?limit=50"), route("GET", "/channels/6/messages?limit=50"));
        assert_ne!(route("GET", "/channels/5/messages"), route("POST", "/channels/5/messages"));
    }

    #[test]
    fn a_429_that_is_not_discords_json_is_read_from_its_headers() {
        let headers = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(known, _)| *known == name).map(|(_, value)| (*value).to_owned())
        };
        // Cloudflare: a page, a Retry-After, nothing Discord wrote. Global, and counts as ours.
        let edge = limit(&Value::Null, headers(&[("retry-after", "64")]));
        assert_eq!((edge.wait, edge.global, edge.shared), (64., true, false));
        // Discord's own, with the body lost: its scope says who is limited.
        let user = limit(&Value::Null, headers(&[("retry-after", "2"), ("x-ratelimit-scope", "user")]));
        assert_eq!((user.wait, user.global), (2., false));
        let global = limit(&Value::Null, headers(&[("retry-after", "3"), ("x-ratelimit-scope", "global")]));
        assert!(global.global);
        let shared = limit(&json!({"retry_after": 0.5, "global": false}), headers(&[("x-ratelimit-scope", "shared")]));
        assert_eq!((shared.wait, shared.global, shared.shared), (0.5, false, true));
        assert!(limit(&json!({"retry_after": 1.0, "global": true}), headers(&[])).global);
        // A header that is not a number of seconds is no wait at all, and never panics.
        for odd in ["NaN", "inf", "-inf", " nan "] {
            assert_eq!(seconds_of(odd), None, "{odd}");
            let odd = limit(&Value::Null, move |name: &str| (name == "retry-after").then(|| odd.to_owned()));
            assert!(odd.wait.is_finite());
            let _ = Duration::from_secs_f64(odd.wait.min(3600.));
        }
        assert_eq!(seconds_of(" 1.5 "), Some(1.5));
    }

    #[test]
    fn non_global_429_does_not_set_everything_waits_and_send_caps_retry_to_send_patience() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex as StdMutex};

        let db = std::env::temp_dir().join(format!("tinyclient-discord-rest-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = crate::store::Store::open(&db).unwrap();
        let hub = Arc::new(crate::hub::Hub::new(store));
        let account = protocol::AccountKey { network: protocol::Network::Discord, account: "rest-test".into() };
        let (_adapter, ctx, _rx) = crate::adapters::local(account, hub);

        // Spin up a tiny local HTTP server that serves scripted responses in sequence.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let http_reply = |status: &str, headers: &str, body: &str| {
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        };
        let responses: Arc<StdMutex<VecDeque<String>>> = Arc::new(StdMutex::new(VecDeque::from([
            // 1. Route-scoped non-global 429 on typing -> must NOT set everything_waits.
            http_reply(
                "429 Too Many Requests",
                "X-RateLimit-Scope: user\r\nContent-Type: application/json\r\n",
                r#"{"message":"rl","retry_after":5,"global":false}"#,
            ),
            // 2. Send hits 429 with retry_after > SEND_PATIENCE (15s) -> must fail immediately without sleeping.
            http_reply(
                "429 Too Many Requests",
                "Content-Type: application/json\r\n",
                r#"{"message":"rl","retry_after":15,"global":false}"#,
            ),
            // 3. Send hits short 429 (0.05s <= SEND_PATIENCE) -> retries once and succeeds.
            http_reply(
                "429 Too Many Requests",
                "Content-Type: application/json\r\n",
                r#"{"message":"rl","retry_after":0.05,"global":false}"#,
            ),
            http_reply(
                "200 OK",
                "X-RateLimit-Bucket: shared-bkt\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset-After: 12\r\n",
                "{}",
            ),
            // 4. Explicit `"global": true` 429 -> MUST set everything_waits.
            http_reply(
                "429 Too Many Requests",
                "Content-Type: application/json\r\n",
                r#"{"message":"rl","retry_after":12,"global":true}"#,
            ),
        ])));
        let server_responses = responses.clone();
        let server = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let next = server_responses.lock().unwrap().pop_front();
                match next {
                    Some(reply) => {
                        let _ = stream.write_all(reply.as_bytes());
                    }
                    None => break,
                }
            }
        });

        let rest = Rest::new("tok".to_owned(), ctx);
        let base = format!("http://{addr}");

        // Helper calling against our local server without touching process env.
        let call_local = |method: &str, path: &str, retry_429: bool, max_wait: f64| -> Result<String, Refused> {
            let url = format!("{base}{path}");
            let r = route(method, path);
            let mut waited = false;
            loop {
                rest.pace(&r, max_wait)?;
                let mut response = rest.request(method, &url, None, &[], false).map_err(|e| Refused::Network(e.to_string()))?;
                let headers = response.headers().clone();
                let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
                let bucket = header("x-ratelimit-bucket");
                if let (Some(rem), Some(reset)) = (
                    header("x-ratelimit-remaining").and_then(|v| v.parse::<u32>().ok()),
                    header("x-ratelimit-reset-after").as_deref().and_then(seconds_of),
                ) {
                    rest.remember_window(&r, bucket.as_deref(), Window {
                        remaining: rem,
                        resets: Instant::now() + Duration::from_secs_f64(reset.clamp(0., 3600.)),
                    });
                }
                let status = response.status().as_u16();
                let text = response.body_mut().read_to_string().unwrap_or_default();
                if (200..300).contains(&status) {
                    return Ok(text);
                }
                let problem: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let lim = limit(&problem, header);
                let until = Instant::now() + Duration::from_secs_f64(lim.wait.min(3600.));
                if lim.global {
                    *crate::hub::lock(&rest.everything_waits) = Some(until);
                }
                rest.remember_window(&r, bucket.as_deref(), Window { remaining: 0, resets: until });
                if !retry_429 || waited || lim.wait > max_wait {
                    return Err(Refused::Limited(lim.wait));
                }
                waited = true;
            }
        };

        // 1. Edge 429 on typing: no retry, everything_waits stays None.
        assert!(matches!(call_local("POST", "/channels/1/typing", false, 0.), Err(Refused::Limited(5.0))));
        assert!(crate::hub::lock(&rest.everything_waits).is_none(), "non-global 429 must not set everything_waits");

        // 2. Send hits 15s 429 (> SEND_PATIENCE=8s): returns immediately without sleeping 15s.
        let began = Instant::now();
        assert!(matches!(call_local("POST", "/channels/2/messages", true, SEND_PATIENCE), Err(Refused::Limited(15.0))));
        assert!(began.elapsed() < Duration::from_secs(2), "send must not sleep past SEND_PATIENCE");
        assert!(crate::hub::lock(&rest.everything_waits).is_none());

        // 3. Send hits 0.05s 429 (<= SEND_PATIENCE): retries and succeeds, recording bucket `shared-bkt`.
        assert_eq!(call_local("POST", "/channels/3/messages", true, SEND_PATIENCE).unwrap(), "{}");
        assert_eq!(rest.bucket_key(&route("POST", "/channels/3/messages")), "bucket:shared-bkt");

        // Another route mapped to the same bucket coordinates proactively and respects SEND_PATIENCE.
        rest.remember_window(&route("POST", "/channels/4/messages"), Some("shared-bkt"), Window {
            remaining: 0,
            resets: Instant::now() + Duration::from_secs(12),
        });
        assert!(matches!(rest.pace(&route("POST", "/channels/4/messages"), SEND_PATIENCE), Err(Refused::Limited(_))));

        // 4. Explicit `"global": true` sets everything_waits.
        assert!(matches!(call_local("POST", "/channels/5/messages", true, SEND_PATIENCE), Err(Refused::Limited(12.0))));
        assert!(crate::hub::lock(&rest.everything_waits).is_some(), "global=true 429 must set everything_waits");

        // Wake and end the local server cleanly.
        let _ = std::net::TcpStream::connect(addr);
        let _ = server.join();
    }

    #[test]
    fn every_reaction_in_a_channel_shares_one_window() {
        assert_eq!(
            route("PUT", "/channels/5/messages/77/reactions/%F0%9F%91%8D/@me"),
            route("DELETE", "/channels/5/messages/78/reactions/party:9/@me").replace("DELETE", "PUT"),
        );
    }

    #[test]
    fn a_refusal_reads_as_words_with_the_wait_in_seconds() {
        let slow = Refused::Status { status: 429, code: 20016, message: String::new(), retry_after: Some(7.2) };
        assert_eq!(slow.reason(), "Slow mode: wait 8 s.");
        assert_eq!(Refused::Limited(2.5).reason(), "Rate limited: try again in 3 s.");
        let forbidden = Refused::Status { status: 403, code: 50013, message: "Missing Permissions".into(), retry_after: None };
        assert_eq!(forbidden.reason(), "You do not have permission to do this here.");
        let blocked = Refused::Status { status: 400, code: 200000, message: "automod service blocked".into(), retry_after: None };
        assert_eq!(blocked.reason(), "This server's AutoMod blocked your message.");
        for code in [200000, 200001] {
            let blocked = Refused::Status { status: 400, code, message: "Message blocked by AutoMod".into(), retry_after: None };
            assert_eq!(blocked.reason(), "This server's AutoMod blocked your message.");
        }
    }
}
