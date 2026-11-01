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
//! Personal accounts are not offered: they are another client id, authority and service, and
//! the page is asked for `organizations` only.
//!
//! The session printed: `{"client_id", "oid", "name", "upn", "tenants": [{"id", "name",
//! "refresh_token"}]}`, the user's own tenant first.
//!
//! Ends the process itself when Microsoft refuses: 4 when the organization does not allow
//! this app (the reason on stderr), 6 when the sign-in was refused for another reason.
//! CONTRACT: 6 is new, like Slack's 5; chatd answers "The sign-in window stopped before it
//! finished." for it. And `blocked` in `main.rs` knows only the 53000 codes, so for the
//! others listed in `organization` the window is closed here, 20 seconds after Microsoft's
//! page showed the refusal, instead of staying until the user closes it.

use super::Site;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Teams' own desktop client, the registration third-party clients sign in as.
const CLIENT: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
const LOGIN: &str = "https://login.microsoftonline.com";
/// Work and school accounts of any organization.
const TENANT: &str = "organizations";
const REDIRECT: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";
const SCOPE: &str = "https://api.spaces.skype.com/.default openid profile offline_access";
const AUTHZ: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";
/// Guest tenants tried at one sign-in.
const GUESTS: usize = 8;
const REFUSED: i32 = 6;
/// How long Microsoft's page stays up once it has shown a refusal `main.rs` does not know.
const READ_IT: Duration = Duration::from_secs(20);

/// What one run of the helper made up for its one authorize request.
struct Flow {
    verifier: String,
    state: String,
}

static FLOW: OnceLock<Flow> = OnceLock::new();

/// The page's address and the start of its text, once Microsoft has redirected to the client's
/// own address or shown an error with its code; otherwise null.
const PROBE: &str = "(()=>{try{const u=location.href;\
    const t=document.body?document.body.innerText.slice(0,4000):'';\
    if(u.startsWith('https://login.microsoftonline.com/common/oauth2/nativeclient')||t.includes('AADSTS'))\
    return JSON.stringify({url:u,text:t});return null}catch(e){return null}})()";

/// Microsoft OAuth redirect observed from navigation as well as from the page probe.
pub(super) fn navigated(url: &str) {
    let Some(flow) = FLOW.get() else {
        return;
    };
    match landed(url, &flow.state) {
        Some(Landed::Code(code)) => {
            let probed = signed_in(&code, &flow.verifier);
            super::finish(super::session(probed, None, &super::user_agent()));
        }
        Some(Landed::Refused { error, description }) => refuse(&error, &description),
        None => {}
    }
}

pub(super) fn site() -> Site {
    let flow = FLOW.get_or_init(|| Flow {
        verifier: random(48),
        state: random(16),
    });
    let start = authorize_url(&challenge(&flow.verifier), &flow.state);
    Site {
        title: "Sign in to Teams",
        // One per run of the helper, which is one sign-in.
        start: Box::leak(start.into_boxed_str()),
        probe: PROBE,
        read,
        cookie: None,
        detour: None,
    }
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
fn random(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let read = random_bytes(&mut buffer);
    if let Err(err) = read {
        tracing::warn!("tinyclient-login: no randomness to sign in with: {err}");
        std::process::exit(super::LOAD_FAILED);
    }
    encode(&buffer)
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

fn authorize_url(challenge: &str, state: &str) -> String {
    format!(
        "{LOGIN}/{TENANT}/oauth2/v2.0/authorize?client_id={CLIENT}&response_type=code&redirect_uri={}&scope={}\
         &code_challenge={challenge}&code_challenge_method=S256&state={state}&prompt=select_account",
        escaped(REDIRECT),
        escaped(SCOPE),
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
fn landed(url: &str, state: &str) -> Option<Landed> {
    let rest = url.strip_prefix(REDIRECT)?;
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

fn refuse(error: &str, description: &str) -> ! {
    let code = aadsts(description).unwrap_or(error);
    let recorded = provider_failure(code);
    let class = recorded.split(' ').next().unwrap_or("provider");
    if organization(code) {
        tracing::warn!(network = "teams", class, reason = code, "login failed");
        std::process::exit(super::BLOCKED);
    }
    // Declining the page's own prompt is closing the window by another button.
    if error == "access_denied" && aadsts(description).is_none() {
        std::process::exit(super::CLOSED);
    }
    tracing::warn!(network = "teams", class, reason = code, "login failed");
    std::process::exit(REFUSED);
}

/// What the probe read: Microsoft's redirect, or a page of its own that shows an error.
fn read(found: &str) -> Option<serde_json::Value> {
    let page: serde_json::Value = serde_json::from_str(found).ok()?;
    let (url, text) = (page["url"].as_str().unwrap_or(""), page["text"].as_str().unwrap_or(""));
    let flow = FLOW.get()?;
    match landed(url, &flow.state) {
        Some(Landed::Code(code)) => Some(signed_in(&code, &flow.verifier)),
        Some(Landed::Refused { error, description }) => refuse(&error, &description),
        None => {
            // A refusal shown on Microsoft's own page. The ones `main.rs` knows keep the window
            // open until the user closes it; the rest are given time to be read.
            static SHOWN: OnceLock<Instant> = OnceLock::new();
            let code = aadsts(text).filter(|code| organization(code) && !super::blocked(url, text))?;
            let since = SHOWN.get_or_init(|| {
                tracing::warn!("tinyclient-login: the organization does not allow this sign-in ({code})");
                Instant::now()
            });
            if since.elapsed() >= READ_IT {
                std::process::exit(super::BLOCKED);
            }
            None
        }
    }
}

// After the redirect: Microsoft and Teams are asked directly, as the same browser.

struct Asked {
    status: u16,
    body: serde_json::Value,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
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

fn token(agent: &ureq::Agent, tenant: &str, grant: &[(&str, &str)]) -> Result<Asked, String> {
    let mut form = vec![("client_id", CLIENT), ("scope", SCOPE)];
    form.extend_from_slice(grant);
    asked(agent.post(format!("{LOGIN}/{}/oauth2/v2.0/token", escaped(tenant))).send_form(form))
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
fn session(claims: &serde_json::Value, tenants: &[(String, String, String)]) -> serde_json::Value {
    let tenants: Vec<serde_json::Value> = tenants
        .iter()
        .map(|(id, name, refresh)| serde_json::json!({"id": id, "name": name, "refresh_token": refresh}))
        .collect();
    serde_json::json!({
        "client_id": CLIENT,
        "oid": claims["oid"].as_str().unwrap_or(""),
        "name": claims["name"].as_str().unwrap_or(""),
        "upn": claims["preferred_username"].as_str().unwrap_or(""),
        "tenants": tenants,
    })
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

/// Redeems the code and finds the tenants the sign-in reaches. Ends the helper if Microsoft
/// refuses the code; anything that fails after that only leaves the guest tenants out.
fn signed_in(code: &str, verifier: &str) -> serde_json::Value {
    let agent = agent();
    let grant = [("grant_type", "authorization_code"), ("code", code), ("redirect_uri", REDIRECT), ("code_verifier", verifier)];
    let redeemed = match token(&agent, TENANT, &grant) {
        Ok(redeemed) => redeemed,
        Err(why) => {
            tracing::warn!("tinyclient-login: Microsoft could not be reached to finish the sign-in: {why}");
            std::process::exit(super::LOAD_FAILED);
        }
    };
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_owned();
    let (access, refresh) = (text(&redeemed.body["access_token"]), text(&redeemed.body["refresh_token"]));
    if redeemed.status != 200 || access.is_empty() || refresh.is_empty() {
        let error = text(&redeemed.body["error"]);
        let error = if error.is_empty() { format!("HTTP {}", redeemed.status) } else { error };
        refuse(&error, redeemed.body["error_description"].as_str().unwrap_or("no refresh token was issued"));
    }
    let claims = claims(redeemed.body["id_token"].as_str().unwrap_or(""));
    let home = text(&claims["tid"]);
    if home.is_empty() {
        refuse("no_tenant", "Microsoft did not say which organization signed in");
    }
    // Where this user's Teams is, then which tenants they are in.
    let authz = asked(agent.post(AUTHZ).header("Authorization", &format!("Bearer {access}")).header("Accept", "application/json; ver=1.0").send_empty());
    let middle = authz.ok().and_then(|authz| Some(authz.body["regionGtms"]["middleTier"].as_str()?.trim_end_matches('/').to_owned()));
    // The token goes to Teams' own front door and nowhere else, whatever the answer names.
    let middle = middle.filter(|middle| middle.starts_with("https://teams.microsoft.com/"));
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
        let reached = token(&agent, &id, &[("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())]);
        match reached.ok().filter(|reached| reached.status == 200).map(|reached| text(&reached.body["refresh_token"])) {
            Some(own) if !own.is_empty() => tenants.push((id, name, own)),
            _ => tracing::warn!("tinyclient-login: guest tenant {name:?} did not accept this sign-in and is left out"),
        }
    }
    session(&claims, &tenants)
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
        let (one, other) = (random(48), random(48));
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
}
