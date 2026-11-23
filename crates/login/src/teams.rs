//! Teams sign-in, for work and school accounts: Microsoft's own login page, as Teams' own client.
//!
//! The window opens Microsoft's authorize page (passwords, MFA, passkeys and an organization's
//! SSO are that page's own) for the client id working third-party clients sign in as, with the
//! native-client redirect it registered. Microsoft sends the window to that address with a
//! code; the code is redeemed here for a refresh token, which is what chatd keeps. A token
//! lifted out of the Teams web app instead would be a single-page-app token that ends within a
//! day.
//!
//! Evidence, read 2026-10-13 (see the notes in `chatd/src/adapters/teams.rs`): the client id,
//! the redirect and the code exchange at `/oauth2/v2.0/token` are purple-teams' and ost's. PKCE
//! is not sent by either; it is standard for a public client and unconfirmed for this one. The
//! tenant list (`{middleTier}/beta/users/tenants`) is mautrix-teams'. Reaching a guest tenant
//! with the same refresh token is in no client's code as such; it is tried here, while the
//! user is at the window, and a tenant that refuses is left out.
//!
//! A personal Microsoft account is a separate sign-in (`teams-personal`): client id
//! `8ec6bc83-69c8-4392-8f08-b3c986009232`, authority `consumers`, and the consumer scope.
//! Work and school accounts stay on `organizations`.
//!
//! The session printed: `{"client_id", "oid", "name", "upn", "tenants": [{"id", "name",
//! "refresh_token"}]}`, the user's own tenant first.
//!
//! A refusal from Microsoft ends the sign-in (`Flow`; the helper exits with it): 4 when the organization does not allow
//! this app (the reason on stderr), 6 when the sign-in was refused for another reason.
//! CONTRACT: 6 is new, like Slack's 5; chatd answers "The sign-in window stopped before it
//! finished." for it. And `blocked` in `main.rs` knows only the 53000 codes, so for the
//! others listed in `organization` the window is closed here, 20 seconds after Microsoft's
//! page showed the refusal, instead of staying until the user closes it.

use super::Site;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Teams' own desktop client, the registration third-party clients sign in as.
const CLIENT: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
/// The client a personal Microsoft account signs in as.
const PERSONAL_CLIENT: &str = "8ec6bc83-69c8-4392-8f08-b3c986009232";
/// Work and school accounts of any organization.
const TENANT: &str = "organizations";
/// The one scope a personal account asks for.
const PERSONAL_SCOPE: &str = "https://mtsvc.fl.teams.microsoft.com/teams.mt.readwrite openid profile offline_access";
/// Guest tenants tried at one sign-in.
const GUESTS: usize = 8;
const REFUSED: i32 = 6;
/// How long Microsoft's page stays up once it has shown a refusal `main.rs` does not know.
const READ_IT: Duration = Duration::from_secs(20);

/// Which Microsoft 365 cloud a Teams sign-in targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cloud {
    #[default]
    Worldwide,
    GccHigh,
    Dod,
    China,
    /// A personal Microsoft account (Teams free).
    Personal,
}

impl Cloud {
    pub fn parse(raw: &str) -> Cloud {
        match raw.trim().to_ascii_lowercase().as_str() {
            "gcc_high" | "gcchigh" | "gcc-high" | "usgov" | "usgov_high" | "usgovhigh" => Cloud::GccHigh,
            "dod" | "usgov_dod" | "usgovdod" | "usdod" => Cloud::Dod,
            "china" | "21vianet" | "cn" | "gallatin" => Cloud::China,
            "personal" | "consumers" | "live" => Cloud::Personal,
            _ => Cloud::Worldwide,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Cloud::Worldwide => "worldwide",
            Cloud::GccHigh => "gcc_high",
            Cloud::Dod => "dod",
            Cloud::China => "china",
            Cloud::Personal => "personal",
        }
    }

    /// `organizations` for work and school, `consumers` for a personal Microsoft account.
    pub fn authority(self) -> &'static str {
        match self {
            Cloud::Personal => "consumers",
            Cloud::Worldwide | Cloud::GccHigh | Cloud::Dod | Cloud::China => TENANT,
        }
    }

    pub fn client_id(self) -> &'static str {
        match self {
            Cloud::Personal => PERSONAL_CLIENT,
            Cloud::Worldwide | Cloud::GccHigh | Cloud::Dod | Cloud::China => CLIENT,
        }
    }

    pub fn login(self) -> &'static str {
        match self {
            Cloud::Worldwide | Cloud::Personal => "https://login.microsoftonline.com",
            Cloud::GccHigh | Cloud::Dod => "https://login.microsoftonline.us",
            Cloud::China => "https://login.chinacloudapi.cn",
        }
    }

    pub fn teams(self) -> &'static str {
        match self {
            Cloud::Worldwide => "https://teams.microsoft.com",
            Cloud::GccHigh => "https://gov.teams.microsoft.us",
            Cloud::Dod => "https://dod.teams.microsoft.us",
            Cloud::China => "https://teams.microsoftonline.cn",
            Cloud::Personal => "https://teams.live.com",
        }
    }

    pub fn redirect(self) -> &'static str {
        match self {
            Cloud::Worldwide | Cloud::Personal => "https://login.microsoftonline.com/common/oauth2/nativeclient",
            Cloud::GccHigh | Cloud::Dod => "https://login.microsoftonline.us/common/oauth2/nativeclient",
            Cloud::China => "https://login.chinacloudapi.cn/common/oauth2/nativeclient",
        }
    }

    pub fn scope(self) -> &'static str {
        match self {
            Cloud::Worldwide => "https://api.spaces.skype.com/.default openid profile offline_access",
            Cloud::GccHigh => "https://gov.teams.microsoft.us/.default openid profile offline_access",
            Cloud::Dod => "https://dod.teams.microsoft.us/.default openid profile offline_access",
            Cloud::China => "https://teams.microsoftonline.cn/.default openid profile offline_access",
            Cloud::Personal => PERSONAL_SCOPE,
        }
    }

    pub fn authz(self) -> String {
        if self == Cloud::Personal {
            format!("{}/api/auth/v1.0/authz/consumer", self.teams())
        } else {
            format!("{}/api/authsvc/v1.0/authz", self.teams())
        }
    }

    /// Detects the Microsoft 365 cloud from a user-realm (`getuserrealm.srf` / `/common/userrealm/`)
    /// or OpenID discovery JSON payload, with an optional email/domain hint for DoD disambiguation.
    pub fn from_discovery(doc: &serde_json::Value, hint: Option<&str>) -> Cloud {
        let candidates = [
            doc["cloud_instance_name"].as_str(),
            doc["CloudInstanceName"].as_str(),
            doc["tenant_region_scope"].as_str(),
            doc["issuer"].as_str(),
            doc["authorization_endpoint"].as_str(),
            doc["AuthURL"].as_str(),
            doc["FederationBrandName"].as_str(),
        ];
        let hint_lower = hint.unwrap_or("").to_ascii_lowercase();
        let is_dod_hint = hint_lower.ends_with(".mil") || hint_lower.contains("dod.teams.microsoft.us");
        for raw in candidates.into_iter().flatten() {
            let lower = raw.to_ascii_lowercase();
            if lower.contains("chinacloudapi.cn") || lower.contains("microsoftonline.cn") || lower.contains("teams.microsoft.cn") || lower == "china" {
                return Cloud::China;
            }
            if lower.contains("dod.teams.microsoft.us") || lower.contains("dod-graph.microsoft.us") || lower.contains("usdod") {
                return Cloud::Dod;
            }
            if lower.contains("microsoftonline.us") || lower.contains("gov.teams.microsoft.us") || lower.contains("usgov") {
                return if is_dod_hint { Cloud::Dod } else { Cloud::GccHigh };
            }
        }
        if is_dod_hint {
            return Cloud::Dod;
        }
        Cloud::Worldwide
    }

    /// Infers the cloud from an ID token's `iss` claim, falling back to `fallback`.
    pub fn from_claims(claims: &serde_json::Value, fallback: Cloud) -> Cloud {
        let detected = Self::from_discovery(claims, claims["preferred_username"].as_str());
        if detected != Cloud::Worldwide {
            detected
        } else {
            fallback
        }
    }
}

const REDIRECTS: &[&str] = &[
    "https://login.microsoftonline.com/common/oauth2/nativeclient",
    "https://login.microsoftonline.us/common/oauth2/nativeclient",
    "https://login.chinacloudapi.cn/common/oauth2/nativeclient",
];

/// The secrets one sign-in made up for its one authorize request, and what it has seen.
pub struct Flow {
    cloud: Cloud,
    verifier: String,
    state: String,
    /// When Microsoft's own page first showed a refusal `blocked` does not know.
    shown: Mutex<Option<Instant>>,
}

/// How a Teams sign-in ended, when it has: the session, or the helper's exit code for why not.
pub type Ending = Result<serde_json::Value, i32>;

/// The page's address and the start of its text, once Microsoft has redirected to the client's
/// own address or shown an error with its code; otherwise null.
pub const PROBE: &str = "(()=>{try{const u=location.href;\
    const t=document.body?document.body.innerText.slice(0,4000):'';\
    if(u.startsWith('https://login.microsoftonline.com/common/oauth2/nativeclient')\
    ||u.startsWith('https://login.microsoftonline.us/common/oauth2/nativeclient')\
    ||u.startsWith('https://login.chinacloudapi.cn/common/oauth2/nativeclient')\
    ||t.includes('AADSTS'))\
    return JSON.stringify({url:u,text:t});return null}catch(e){return null}})()";

pub const TITLE: &str = "Sign in to Teams";

impl Flow {
    /// A fresh verifier and state. Uses `TINYCLIENT_TEAMS_CLOUD` when set, otherwise Worldwide.
    /// `Err` with the exit code when the system has no randomness.
    pub fn new() -> Result<Flow, i32> {
        let cloud = std::env::var("TINYCLIENT_TEAMS_CLOUD")
            .ok()
            .map(|raw| Cloud::parse(&raw))
            .unwrap_or(Cloud::Worldwide);
        Self::for_cloud(cloud)
    }

    /// A fresh sign-in flow targeting a specific Microsoft 365 cloud.
    pub fn for_cloud(cloud: Cloud) -> Result<Flow, i32> {
        let made = random(48).and_then(|verifier| Ok((verifier, random(16)?)));
        match made {
            Ok((verifier, state)) => Ok(Flow { cloud, verifier, state, shown: Mutex::new(None) }),
            Err(err) => {
                tracing::warn!("tinyclient-login: no randomness to sign in with: {err}");
                Err(super::LOAD_FAILED)
            }
        }
    }

    /// Microsoft's authorize page for this sign-in.
    pub fn start(&self) -> String {
        authorize_url_for(self.cloud, &challenge(&self.verifier), &self.state)
    }

    /// True for a navigation to the redirect, which the page cannot show and need not load.
    pub fn is_redirect(url: &str) -> bool {
        REDIRECTS.iter().any(|prefix| url.starts_with(prefix))
    }

    /// A navigation the page made. `Some` once it is Microsoft's redirect for this sign-in: the
    /// code redeemed for the session (blocking, over the network), or why not.
    pub fn navigated(&self, url: &str) -> Option<Ending> {
        match landed_for(self.cloud, url, &self.state)? {
            Landed::Code(code) => Some(signed_in_for(self.cloud, &code, &self.verifier)),
            Landed::Refused { error, description } => Some(Err(refusal(&error, &description))),
        }
    }

    /// What `PROBE` read: Microsoft's redirect, or a page of its own that shows an error.
    pub fn read(&self, found: &str) -> Option<Ending> {
        let page: serde_json::Value = serde_json::from_str(found).ok()?;
        let (url, text) = (page["url"].as_str().unwrap_or(""), page["text"].as_str().unwrap_or(""));
        if let Some(ending) = self.navigated(url) {
            return Some(ending);
        }
        // A refusal shown on Microsoft's own page. The ones `blocked` knows keep the window open
        // until the user closes it; the rest are given time to be read.
        let code = aadsts(text).filter(|code| organization(code) && !super::blocked(url, text))?;
        let mut shown = self.shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let since = shown.get_or_insert_with(|| {
            tracing::warn!("tinyclient-login: the organization does not allow this sign-in ({code})");
            Instant::now()
        });
        (since.elapsed() >= READ_IT).then_some(Err(super::BLOCKED))
    }
}

// The helper binary's one sign-in, which ends the process when it ends.

static FLOW: OnceLock<Flow> = OnceLock::new();

fn end(ending: Ending) -> ! {
    match ending {
        Ok(probed) => super::finish(super::session(probed, None, &super::user_agent())),
        Err(code) => std::process::exit(code),
    }
}

/// Microsoft OAuth redirect observed from navigation as well as from the page probe. For the
/// helper binary: ends the process once the sign-in has ended.
pub fn navigated(url: &str) {
    if let Some(ending) = FLOW.get().and_then(|flow| flow.navigated(url)) {
        end(ending);
    }
}

fn site_of(flow: &Flow) -> Site {
    Site {
        title: TITLE,
        // One per run of the helper, which is one sign-in.
        start: Box::leak(flow.start().into_boxed_str()),
        probe: PROBE,
        read,
        cookie: None,
        detour: None,
    }
}

/// The helper binary's Teams site. Ends the process if no sign-in can be made.
pub fn site() -> Site {
    let flow = FLOW.get_or_init(|| Flow::new().unwrap_or_else(|code| std::process::exit(code)));
    site_of(flow)
}

/// A personal Microsoft account: `consumers` and teams.live.com, not a work tenant.
pub fn site_personal() -> Site {
    let flow = FLOW.get_or_init(|| Flow::for_cloud(Cloud::Personal).unwrap_or_else(|code| std::process::exit(code)));
    site_of(flow)
}

/// `Site::read` for the helper binary.
fn read(found: &str) -> Option<serde_json::Value> {
    let ending = FLOW.get()?.read(found)?;
    end(ending)
}

// Small things the standard library does not have.

const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64 in the URL alphabet, without padding.
fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (at, byte)| word | u32::from(*byte) << (16 - 8 * at));
        for digit in 0..=chunk.len() {
            out.push(URL_SAFE[(word >> (18 - 6 * digit) & 63) as usize] as char);
        }
    }
    out
}

/// Reads base64 in either alphabet, padded or not.
fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut word, mut bits) = (0u32, 0);
    for byte in text.bytes().filter(|byte| *byte != b'=') {
        let digit = match byte {
            b'+' => 62,
            b'/' => 63,
            byte => URL_SAFE.iter().position(|known| *known == byte)? as u32,
        };
        word = word << 6 | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((word >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(target_os = "macos")]
fn sha256(data: &[u8]) -> [u8; 32] {
    #[link(name = "System")]
    extern "C" {
        fn CC_SHA256(data: *const u8, len: u32, digest: *mut u8) -> *mut u8;
    }
    let mut digest = [0u8; 32];
    // SAFETY: CommonCrypto reads `len` bytes of `data` and writes 32 bytes to `digest`.
    unsafe { CC_SHA256(data.as_ptr(), data.len() as u32, digest.as_mut_ptr()) };
    digest
}

#[cfg(not(target_os = "macos"))]
fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

/// `bytes` bytes nobody can guess, in the URL alphabet. There is no sign-in without them.
fn random(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0u8; bytes];
    random_bytes(&mut buffer)?;
    Ok(encode(&buffer))
}

fn random_bytes(buffer: &mut [u8]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(buffer))
            .map_err(|err| err.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        getrandom::fill(buffer).map_err(|err| err.to_string())
    }
}

/// The PKCE challenge for a verifier: the hash, not the verifier, goes in the address bar.
fn challenge(verifier: &str) -> String {
    encode(&sha256(verifier.as_bytes()))
}

fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn unescaped(text: &str) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut at) = (Vec::with_capacity(bytes.len()), 0);
    while at < bytes.len() {
        let hex = bytes.get(at + 1..at + 3).and_then(|hex| std::str::from_utf8(hex).ok());
        match (bytes[at], hex.and_then(|hex| u8::from_str_radix(hex, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 2;
            }
            (b'+', _) => out.push(b' '),
            (byte, _) => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// The sign-in.

#[cfg(test)]
fn authorize_url(challenge: &str, state: &str) -> String {
    authorize_url_for(Cloud::Worldwide, challenge, state)
}

fn authorize_url_for(cloud: Cloud, challenge: &str, state: &str) -> String {
    format!(
        "{}/{}/oauth2/v2.0/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}\
         &code_challenge={challenge}&code_challenge_method=S256&state={state}&prompt=select_account",
        cloud.login(),
        cloud.authority(),
        cloud.client_id(),
        escaped(cloud.redirect()),
        escaped(cloud.scope()),
    )
}

/// How Microsoft ended the visit to its pages.
#[derive(Debug, PartialEq)]
enum Landed {
    /// Signed in: the code to redeem.
    Code(String),
    /// Refused: OAuth's error and Microsoft's own description, which starts with its code.
    Refused { error: String, description: String },
}

/// Reads the address Microsoft redirected to. `None` for any other address, and for a redirect
/// that does not answer this run's request (another window's, or a forged one).
#[cfg(test)]
fn landed(url: &str, state: &str) -> Option<Landed> {
    landed_for(Cloud::Worldwide, url, state)
}

fn landed_for(cloud: Cloud, url: &str, state: &str) -> Option<Landed> {
    let rest = url.strip_prefix(cloud.redirect())?;
    let query = rest.strip_prefix('?').or_else(|| rest.strip_prefix('#'))?;
    let query = query.split('#').next().unwrap_or(query);
    let value = |name: &str| {
        query.split('&').filter_map(|pair| pair.split_once('=')).find(|(known, _)| *known == name).map(|(_, value)| unescaped(value))
    };
    if let Some(error) = value("error") {
        return Some(Landed::Refused {
            error,
            description: value("error_description").unwrap_or_default(),
        });
    }
    let code = value("code").filter(|code| !code.is_empty())?;
    (value("state").as_deref() == Some(state)).then_some(Landed::Code(code))
}

/// Microsoft's error code in a page's text or an error's description: `AADSTS53003`.
fn aadsts(text: &str) -> Option<&str> {
    let at = text.find("AADSTS")?;
    let digits = text[at + 6..].bytes().take_while(u8::is_ascii_digit).count();
    (digits >= 4).then(|| &text[at..at + 6 + digits])
}

/// True for the codes that mean the user's organization does not allow this app or this
/// device, which signing in again cannot change. Only their administrators can.
fn organization(code: &str) -> bool {
    matches!(
        code,
        // Conditional access: a managed or compliant device, an approved app, a location.
        "AADSTS53000" | "AADSTS53001" | "AADSTS53002" | "AADSTS53003" | "AADSTS53004" | "AADSTS530002" | "AADSTS530003"
        // The user is not assigned to the app, or the app is disabled or absent in the tenant.
        | "AADSTS50105" | "AADSTS7000112" | "AADSTS700016"
        // Consent the user cannot give, or that an administrator has not given.
        | "AADSTS65001" | "AADSTS65004" | "AADSTS90094" | "AADSTS900941"
        // Blocked by the tenant's own policy on tokens, flows or sign-in risk.
        | "AADSTS50158" | "AADSTS530032" | "AADSTS530034" | "AADSTS50199"
    )
}

/// Ends the helper because Microsoft refused the sign-in, saying which way on stderr.
/// Static class and the OAuth error code. Microsoft's sentence is not included.
fn provider_failure(code: &str) -> String {
    let class = if organization(code) { "policy" } else { "provider" };
    format!("{class} {code}")
}

/// The exit code for a refusal from Microsoft, logged with its class and code.
fn refusal(error: &str, description: &str) -> i32 {
    let code = aadsts(description).unwrap_or(error);
    let recorded = provider_failure(code);
    let class = recorded.split(' ').next().unwrap_or("provider");
    if organization(code) {
        tracing::warn!(network = "teams", class, reason = code, "login failed");
        return super::BLOCKED;
    }
    // Declining the page's own prompt is closing the window by another button.
    if error == "access_denied" && aadsts(description).is_none() {
        return super::CLOSED;
    }
    tracing::warn!(network = "teams", class, reason = code, "login failed");
    REFUSED
}

// After the redirect: Microsoft and Teams are asked directly, as the same browser.

struct Asked {
    status: u16,
    body: serde_json::Value,
}

/// The system's TLS and roots; on Android, which has none this crate links, rustls and the
/// bundled roots (as `chatd::tls`).
#[cfg(not(target_os = "android"))]
fn tls() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build()
}

#[cfg(target_os = "android")]
fn tls() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::Rustls).build()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(tls())
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent(super::user_agent())
        .build()
        .into()
}

fn asked(sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Asked, String> {
    let mut response = sent.map_err(|err| err.to_string())?;
    Ok(Asked {
        status: response.status().as_u16(),
        body: response.body_mut().read_json().unwrap_or(serde_json::Value::Null),
    })
}

fn token(agent: &ureq::Agent, cloud: Cloud, tenant: &str, grant: &[(&str, &str)]) -> Result<Asked, String> {
    let mut form = vec![("client_id", cloud.client_id()), ("scope", cloud.scope())];
    form.extend_from_slice(grant);
    asked(agent.post(format!("{}/{}/oauth2/v2.0/token", cloud.login(), escaped(tenant))).send_form(form))
}

/// The claims of an id token. Nothing is verified: it came straight from Microsoft over TLS,
/// and is read only for who signed in.
fn claims(id_token: &str) -> serde_json::Value {
    let payload = id_token.split('.').nth(1).and_then(decode);
    payload.and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(serde_json::Value::Null)
}

/// The tenants the user is in besides their own, as the middle tier lists them. A guest who
/// never accepted the invitation is not in that tenant yet.
fn guests(listed: &serde_json::Value, home: &str) -> Vec<(String, String)> {
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_owned();
    let listed = listed.as_array().map_or(&[][..], Vec::as_slice);
    let redeemed = |tenant: &&serde_json::Value| tenant["isInvitationRedeemed"] != false && tenant["tenantId"] != home && !text(&tenant["tenantId"]).is_empty();
    listed.iter().filter(redeemed).take(GUESTS).map(|tenant| (text(&tenant["tenantId"]), text(&tenant["tenantName"]))).collect()
}

/// The session as chatd stores it.
fn session_for(cloud: Cloud, claims: &serde_json::Value, tenants: &[(String, String, String)]) -> serde_json::Value {
    let tenants: Vec<serde_json::Value> = tenants
        .iter()
        .map(|(id, name, refresh)| {
            let mut entry = serde_json::json!({"id": id, "name": name, "refresh_token": refresh});
            if cloud == Cloud::Personal {
                entry["client_id"] = serde_json::json!(cloud.client_id());
                entry["personal"] = serde_json::json!(true);
                entry["cloud"] = serde_json::json!("personal");
            }
            entry
        })
        .collect();
    let mut out = serde_json::json!({
        "client_id": cloud.client_id(),
        "oid": claims["oid"].as_str().unwrap_or(""),
        "name": claims["name"].as_str().unwrap_or(""),
        "upn": claims["preferred_username"].as_str().unwrap_or(""),
        "tenants": tenants,
    });
    if cloud != Cloud::Worldwide {
        out["cloud"] = serde_json::Value::String(cloud.as_str().to_owned());
    }
    if cloud == Cloud::Personal {
        out["personal"] = serde_json::json!(true);
    }
    out
}

#[cfg(test)]
fn session(claims: &serde_json::Value, tenants: &[(String, String, String)]) -> serde_json::Value {
    session_for(Cloud::Worldwide, claims, tenants)
}

/// A tenant's name when Teams did not give one: the organization's part of the sign-in name.
fn fallback_name(upn: &str) -> String {
    let domain = upn.rsplit_once('@').map_or("", |(_, domain)| domain);
    let name = domain.split('.').next().unwrap_or("");
    if name.is_empty() {
        return "Teams".to_owned();
    }
    let mut letters = name.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

/// Queries Microsoft's user-realm discovery endpoint for an email or domain to identify whether
/// the organization lives in Worldwide, GCC High, DoD, or China (21Vianet).
pub fn discover_cloud(hint: &str) -> Cloud {
    let hint = hint.trim();
    if hint.is_empty() {
        return Cloud::Worldwide;
    }
    let url = format!("https://login.microsoftonline.com/common/userrealm/{}?api-version=2.0", escaped(hint));
    let agent = agent();
    match asked(agent.get(&url).call()) {
        Ok(ans) if ans.status == 200 => Cloud::from_discovery(&ans.body, Some(hint)),
        _ => Cloud::from_discovery(&serde_json::Value::Null, Some(hint)),
    }
}

/// Redeems the code and finds the tenants the sign-in reaches. `Err` with the exit code if Microsoft
/// refuses the code; anything that fails after that only leaves the guest tenants out.
fn signed_in_for(cloud: Cloud, code: &str, verifier: &str) -> Ending {
    let agent = agent();
    let grant = [("grant_type", "authorization_code"), ("code", code), ("redirect_uri", cloud.redirect()), ("code_verifier", verifier)];
    let redeemed = match token(&agent, cloud, cloud.authority(), &grant) {
        Ok(redeemed) => redeemed,
        Err(why) => {
            tracing::warn!("tinyclient-login: Microsoft could not be reached to finish the sign-in: {why}");
            return Err(super::LOAD_FAILED);
        }
    };
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_owned();
    let (access, refresh) = (text(&redeemed.body["access_token"]), text(&redeemed.body["refresh_token"]));
    if redeemed.status != 200 || access.is_empty() || refresh.is_empty() {
        let error = text(&redeemed.body["error"]);
        let error = if error.is_empty() { format!("HTTP {}", redeemed.status) } else { error };
        return Err(refusal(&error, redeemed.body["error_description"].as_str().unwrap_or("no refresh token was issued")));
    }
    let claims = claims(redeemed.body["id_token"].as_str().unwrap_or(""));
    let home = text(&claims["tid"]);
    if home.is_empty() {
        return Err(refusal("no_tenant", "Microsoft did not say which organization signed in"));
    }
    let cloud = Cloud::from_claims(&claims, cloud);
    // Where this user's Teams is, then which tenants they are in.
    let authz = asked(agent.post(cloud.authz()).header("Authorization", &format!("Bearer {access}")).header("Accept", "application/json; ver=1.0").send_empty());
    let middle = authz.ok().and_then(|authz| Some(authz.body["regionGtms"]["middleTier"].as_str()?.trim_end_matches('/').to_owned()));
    // The token goes to this cloud's Teams front door and nowhere else, whatever the answer names.
    let teams_prefix = format!("{}/", cloud.teams());
    let middle = middle.filter(|middle| middle.starts_with(&teams_prefix));
    let listed = middle.and_then(|middle| {
        let sent = agent.get(format!("{middle}/beta/users/tenants")).header("Authorization", &format!("Bearer {access}")).call();
        asked(sent).ok().filter(|listed| listed.status == 200).map(|listed| listed.body)
    });
    let listed = listed.unwrap_or(serde_json::Value::Null);
    let named = listed.as_array().into_iter().flatten().find(|tenant| tenant["tenantId"] == home.as_str());
    let name = named.map(|tenant| text(&tenant["tenantName"])).filter(|name| !name.is_empty());
    let name = name.unwrap_or_else(|| fallback_name(claims["preferred_username"].as_str().unwrap_or("")));
    let mut tenants = vec![(home.clone(), name, refresh.clone())];
    for (id, name) in guests(&listed, &home) {
        // A guest tenant with rules of its own (its own MFA, its own device policy) refuses
        // here; it is left out rather than shown as an account that never connects.
        let reached = token(&agent, cloud, &id, &[("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())]);
        match reached.ok().filter(|reached| reached.status == 200).map(|reached| text(&reached.body["refresh_token"])) {
            Some(own) if !own.is_empty() => tenants.push((id, name, own)),
            _ => tracing::warn!("tinyclient-login: guest tenant {name:?} did not accept this sign-in and is left out"),
        }
    }
    Ok(session_for(cloud, &claims, &tenants))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_authorize_request_is_a_public_client_asking_for_a_code_with_pkce() {
        // RFC 7636's own example pair.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let url = authorize_url("CHALLENGE", "STATE");
        assert!(url.starts_with("https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=1fec8e78-bce4-4aaf-ab1b-5451cc387264&response_type=code&"));
        assert!(url.contains("&redirect_uri=https%3A%2F%2Flogin.microsoftonline.com%2Fcommon%2Foauth2%2Fnativeclient&"));
        assert!(url.contains("&scope=https%3A%2F%2Fapi.spaces.skype.com%2F.default%20openid%20profile%20offline_access&"));
        assert!(url.contains("&code_challenge=CHALLENGE&code_challenge_method=S256&state=STATE&"));
        assert!(!url.contains("client_secret") && !url.contains(' '));
        // Each run makes its own secrets, long enough to be a verifier.
        let (one, other) = (random(48).unwrap(), random(48).unwrap());
        assert_eq!(one.len(), 64);
        assert!(one != other && one.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    }

    #[test]
    fn the_redirect_is_captured_only_when_it_answers_this_request() {
        let redirect = |query: &str| format!("https://login.microsoftonline.com/common/oauth2/nativeclient{query}");
        let code = "?code=0.AXkA%2Babc_def-123&client_info=eyJ1aWQiOiJ4In0&state=STATE&session_state=7d1a";
        assert_eq!(landed(&redirect(code), "STATE"), Some(Landed::Code("0.AXkA+abc_def-123".to_owned())));
        // Someone else's redirect, a code with no state, and no code at all are not ours.
        assert_eq!(landed(&redirect(code), "OTHER"), None);
        assert_eq!(landed(&redirect("?code=abc"), "STATE"), None);
        assert_eq!(landed(&redirect("?state=STATE"), "STATE"), None);
        assert_eq!(landed(&redirect(""), "STATE"), None);
        // Every page before the redirect, and a look-alike of it.
        assert_eq!(landed("https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x&state=STATE", "STATE"), None);
        assert_eq!(landed("https://login.microsoftonline.com/common/login", "STATE"), None);
        assert_eq!(landed("https://evil.example/common/oauth2/nativeclient?code=abc&state=STATE", "STATE"), None);
        assert_eq!(landed("https://evil.example/?next=https://login.microsoftonline.com/common/oauth2/nativeclient?code=a&state=STATE", "STATE"), None);
    }

    #[test]
    fn a_refusal_on_the_redirect_carries_microsofts_code() {
        let url = "https://login.microsoftonline.com/common/oauth2/nativeclient?error=access_denied\
                   &error_description=AADSTS53003%3a+Access+has+been+blocked+by+Conditional+Access+policies.+The+access+policy+does+not+allow+token+issuance.%0d%0aTrace+ID%3a+0a1b&state=STATE";
        let Some(Landed::Refused { error, description }) = landed(url, "STATE") else {
            panic!("not read as a refusal");
        };
        assert_eq!(error, "access_denied");
        assert!(description.starts_with("AADSTS53003: Access has been blocked by Conditional Access policies."));
        assert_eq!(aadsts(&description), Some("AADSTS53003"));
        // A refusal is read whatever state it carries: there is nothing to redeem.
        assert!(matches!(landed("https://login.microsoftonline.com/common/oauth2/nativeclient?error=server_error", "STATE"), Some(Landed::Refused { .. })));
    }

    #[test]
    fn an_organizations_refusal_is_told_from_one_signing_in_again_can_fix() {
        for blocked in ["AADSTS53003", "AADSTS53000", "AADSTS530003", "AADSTS50105", "AADSTS65001", "AADSTS90094", "AADSTS50158"] {
            assert!(organization(blocked), "{blocked}");
        }
        // A wrong password, an expired code, MFA not finished, a cancelled prompt.
        for other in ["AADSTS50126", "AADSTS70008", "AADSTS50076", "AADSTS50058", "access_denied", ""] {
            assert!(!organization(other), "{other}");
        }
        assert_eq!(aadsts("Sorry, but we're having trouble signing you in.\nAADSTS50105: Your administrator has configured the application"), Some("AADSTS50105"));
        assert_eq!(aadsts("Error code: AADSTS7000112 (application disabled)"), Some("AADSTS7000112"));
        assert_eq!((aadsts("Sign in to your account"), aadsts("AADSTS"), aadsts("AADSTS12 is too short")), (None, None, None));
    }

    #[test]
    fn provider_failure_keeps_the_code_and_drops_the_description() {
        let line = provider_failure("access_denied");
        assert!(line.contains("access_denied"));
        assert!(!line.contains("error_description"));
        assert!(provider_failure("AADSTS53003").starts_with("policy "));
    }

    #[test]
    fn a_page_showing_a_refusal_is_reported_and_an_ordinary_page_is_not() {
        // Nothing to read before the helper has made its request, or on a page that is not JSON.
        assert_eq!(read("not json"), None);
        assert_eq!(read(r#"{"url":"https://login.microsoftonline.com/common/login","text":"Enter password"}"#), None);
    }

    #[test]
    fn who_signed_in_is_read_from_the_id_token() {
        let payload = encode(br#"{"tid":"1111-aaaa","oid":"2222-bbbb","name":"Ana Ruiz","preferred_username":"ana@contoso.com"}"#);
        let read = claims(&format!("eyJhbGciOiJSUzI1NiJ9.{payload}.c2lnbmF0dXJl"));
        assert_eq!((read["tid"].as_str(), read["oid"].as_str(), read["name"].as_str()), (Some("1111-aaaa"), Some("2222-bbbb"), Some("Ana Ruiz")));
        assert_eq!(claims("opaque"), serde_json::Value::Null);
        // Base64 as Microsoft writes it (padded or not, either alphabet) reads back.
        for text in ["", "a", "ab", "abc", "any carnal pleas", "\u{1f600}?>~"] {
            assert_eq!(decode(&encode(text.as_bytes())).as_deref(), Some(text.as_bytes()), "{text:?}");
        }
        assert_eq!(decode("YW55IGNhcm5hbCBwbGVhcw==").as_deref(), Some(&b"any carnal pleas"[..]));
        assert_eq!(decode("Pz4+fg").as_deref(), Some(&b"?>>~"[..]));
        assert_eq!(decode("not base64!"), None);
    }

    #[test]
    fn the_session_is_one_entry_per_tenant_reached_with_the_users_own_first() {
        let listed = json!([
            {"tenantId": "1111-aaaa", "tenantName": "Contoso", "userType": "member", "isInvitationRedeemed": true},
            {"tenantId": "3333-cccc", "tenantName": "Fabrikam", "userType": "guest", "isInvitationRedeemed": true},
            {"tenantId": "4444-dddd", "tenantName": "Never accepted", "userType": "guest", "isInvitationRedeemed": false},
            {"tenantName": "No id"},
        ]);
        assert_eq!(guests(&listed, "1111-aaaa"), [("3333-cccc".to_owned(), "Fabrikam".to_owned())]);
        assert!(guests(&json!({"error": "nope"}), "1111-aaaa").is_empty());
        let claims = json!({"tid": "1111-aaaa", "oid": "2222-bbbb", "name": "Ana Ruiz", "preferred_username": "ana@contoso.com"});
        let tenants = [("1111-aaaa".to_owned(), "Contoso".to_owned(), "rt-home".to_owned()), ("3333-cccc".to_owned(), "Fabrikam".to_owned(), "rt-guest".to_owned())];
        let made = session(&claims, &tenants);
        assert_eq!(
            made,
            json!({
                "client_id": CLIENT, "oid": "2222-bbbb", "name": "Ana Ruiz", "upn": "ana@contoso.com",
                "tenants": [
                    {"id": "1111-aaaa", "name": "Contoso", "refresh_token": "rt-home"},
                    {"id": "3333-cccc", "name": "Fabrikam", "refresh_token": "rt-guest"},
                ],
            })
        );
        // `main` adds the browser that signed in, as it does for every site.
        assert_eq!(crate::session(made, None, "Safari/26.0")["user_agent"], "Safari/26.0");
        assert_eq!((fallback_name("ana@contoso.com"), fallback_name("ana"), fallback_name("")), ("Contoso".to_owned(), "Teams".to_owned(), "Teams".to_owned()));
    }

    #[test]
    fn sovereign_clouds_are_discovered_and_isolated() {
        assert_eq!(
            Cloud::from_discovery(&json!({"cloud_instance_name": "microsoftonline.us"}), Some("user@agency.gov")),
            Cloud::GccHigh
        );
        assert_eq!(
            Cloud::from_discovery(&json!({"cloud_instance_name": "microsoftonline.us"}), Some("user@unit.mil")),
            Cloud::Dod
        );
        assert_eq!(
            Cloud::from_discovery(&json!({"CloudInstanceName": "partner.microsoftonline.cn", "AuthURL": "https://login.chinacloudapi.cn/"}), None),
            Cloud::China
        );
        assert_eq!(
            Cloud::from_discovery(&json!({"cloud_instance_name": "microsoftonline.com"}), Some("ana@contoso.com")),
            Cloud::Worldwide
        );

        let gcc_url = authorize_url_for(Cloud::GccHigh, "CHALLENGE", "STATE");
        assert!(gcc_url.starts_with("https://login.microsoftonline.us/organizations/oauth2/v2.0/authorize?"));
        assert!(gcc_url.contains("https%3A%2F%2Flogin.microsoftonline.us%2Fcommon%2Foauth2%2Fnativeclient"));
        assert!(gcc_url.contains("https%3A%2F%2Fgov.teams.microsoft.us%2F.default"));
        assert!(!gcc_url.contains("microsoftonline.com") && !gcc_url.contains("skype"));

        let cn_url = authorize_url_for(Cloud::China, "CHALLENGE", "STATE");
        assert!(cn_url.starts_with("https://login.chinacloudapi.cn/organizations/oauth2/v2.0/authorize?"));
        assert!(cn_url.contains("https%3A%2F%2Fteams.microsoftonline.cn%2F.default"));
        assert!(!cn_url.contains("teams.microsoft.cn") && !cn_url.contains("skype"));

        // Redirects match only their own cloud.
        let gcc_redirect = "https://login.microsoftonline.us/common/oauth2/nativeclient?code=gov-code&state=STATE";
        assert!(Flow::is_redirect(gcc_redirect));
        assert_eq!(landed_for(Cloud::GccHigh, gcc_redirect, "STATE"), Some(Landed::Code("gov-code".to_owned())));
        assert_eq!(landed_for(Cloud::Worldwide, gcc_redirect, "STATE"), None);

        let claims = json!({"tid": "1111-gov", "oid": "2222-gov", "name": "Sam Gov", "preferred_username": "sam@agency.gov"});
        let tenants = [("1111-gov".to_owned(), "Agency".to_owned(), "rt-gov".to_owned())];
        let saved = session_for(Cloud::GccHigh, &claims, &tenants);
        assert_eq!(saved["cloud"], "gcc_high");
        assert!(session_for(Cloud::Worldwide, &claims, &tenants).get("cloud").is_none());

        let personal = authorize_url_for(Cloud::Personal, "CHALLENGE", "STATE");
        assert!(personal.contains("/consumers/oauth2/v2.0/authorize?"));
        assert!(personal.contains("client_id=8ec6bc83-69c8-4392-8f08-b3c986009232"));
        assert!(personal.contains("teams.mt.readwrite"));
        assert!(personal.starts_with("https://login.microsoftonline.com/"));
        let saved = session_for(Cloud::Personal, &claims, &tenants);
        assert_eq!(saved["cloud"], "personal");
        assert_eq!(saved["client_id"], PERSONAL_CLIENT);
        assert_eq!(saved["personal"], true);
        assert_eq!(saved["tenants"][0]["client_id"], PERSONAL_CLIENT);
        assert_eq!(saved["tenants"][0]["personal"], true);
        // An MSA issuer does not look like a sovereign cloud, so the personal choice survives.
        assert_eq!(
            Cloud::from_claims(&json!({"iss": "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"}), Cloud::Personal),
            Cloud::Personal
        );
    }
}
