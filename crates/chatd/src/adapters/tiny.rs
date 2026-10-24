//! A Tiny server the owner already runs. One account lives in this network's keychain item.
//! Sign-in is the sheet's (`signin`), never `tinyclient-login`. This slice has no conversations,
//! calls, or media, and does not load a WebRTC stack.

pub mod signin;

use super::{serve_commands, Command, Context, Signed};
use protocol::Link;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use ureq::tls::{TlsConfig, TlsProvider};

/// How long one request may take. A redirect is not followed, so this is one host.
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    base_url: String,
    token: String,
    user_id: String,
    email: String,
    display_name: String,
    role: String,
}

struct Me {
    display_name: String,
    email: String,
}

enum Fault {
    Unauthorized,
    Unreachable,
    /// The server asked for a pause. The password was not checked.
    SlowDown,
    /// The server answered, and the answer was a failure of its own.
    Unavailable,
    BadAnswer,
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn parse(session: &str) -> Result<Saved, String> {
    let saved: Saved = serde_json::from_str(session)
        .map_err(|_| "the Tiny server session is unreadable".to_owned())?;
    if saved.base_url.is_empty() || saved.token.is_empty() || !safe_id(&saved.user_id) {
        return Err("the Tiny server session is incomplete".to_owned());
    }
    if !matches!(saved.role.as_str(), "owner" | "admin" | "member") {
        return Err("the Tiny server session has an unknown role".to_owned());
    }
    Ok(saved)
}

pub fn supported() -> bool {
    true
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let saved = parse(session)?;
    Ok(vec![Signed {
        account: saved.user_id.clone(),
        name: saved.display_name,
        session: session.to_owned(),
    }])
}

pub fn without(session: &str, account: &str) -> Option<String> {
    match parse(session) {
        Ok(saved) if saved.user_id == account => None,
        _ => Some(session.to_owned()),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .build(),
        )
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent("tinyclient")
        .build()
        .into()
}

/// One request. A 3xx is a failure: the password and the bearer stay on the host that was asked.
fn call(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<(u16, Value), Fault> {
    let agent = agent();
    let bearer = token.map(|token| format!("Bearer {token}"));
    let sent = match method {
        "GET" => {
            let mut request = agent.get(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            request.call()
        }
        "DELETE" => {
            let mut request = agent.delete(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            request.call()
        }
        "POST" => {
            let mut request = agent.post(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            match body {
                Some(body) => request.send_json(body),
                None => request.send_empty(),
            }
        }
        _ => return Err(Fault::Unreachable),
    };
    let mut response = sent.map_err(|_| Fault::Unreachable)?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(Fault::Unreachable);
    }
    if status == 204 {
        return Ok((status, Value::Null));
    }
    // The body is not logged: a 200 may hold the bearer.
    let parsed = response.body_mut().read_json().unwrap_or(Value::Null);
    Ok((status, parsed))
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{base}{path}")
}

fn post_session(base: &str, email: &str, password: &str) -> Result<Saved, Fault> {
    let (status, body) = call(
        "POST",
        &endpoint(base, "/v1/session"),
        None,
        Some(serde_json::json!({ "email": email, "password": password })),
    )?;
    match status {
        200 => {}
        401 => return Err(Fault::Unauthorized),
        429 => return Err(Fault::SlowDown),
        500..=599 => return Err(Fault::Unavailable),
        _ => return Err(Fault::Unreachable),
    }
    let answer: SessionAnswer = serde_json::from_value(body).map_err(|_| Fault::BadAnswer)?;
    if answer.token.is_empty()
        || !safe_id(&answer.user_id)
        || !matches!(answer.role.as_str(), "owner" | "admin" | "member")
    {
        return Err(Fault::BadAnswer);
    }
    Ok(Saved {
        base_url: base.to_owned(),
        token: answer.token,
        user_id: answer.user_id,
        email: answer.email,
        display_name: answer.display_name,
        role: answer.role,
    })
}

#[derive(Deserialize)]
struct SessionAnswer {
    token: String,
    user_id: String,
    email: String,
    display_name: String,
    role: String,
}

#[derive(Deserialize, Default)]
struct MeAnswer {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
}

fn get_me(saved: &Saved) -> Result<Me, Fault> {
    let (status, body) = call(
        "GET",
        &endpoint(&saved.base_url, "/v1/me"),
        Some(&saved.token),
        None,
    )?;
    if status == 401 {
        return Err(Fault::Unauthorized);
    }
    if status != 200 {
        return Err(Fault::Unreachable);
    }
    let answer: MeAnswer = serde_json::from_value(body).unwrap_or_default();
    Ok(Me {
        display_name: answer.display_name,
        email: answer.email,
    })
}

fn delete_session(saved: &Saved) {
    let _ = call(
        "DELETE",
        &endpoint(&saved.base_url, "/v1/session"),
        Some(&saved.token),
        None,
    );
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.user_id != ctx.account.account {
        return Err("the Tiny server session is for another account".to_owned());
    }
    let (probe, probe_ctx) = (saved.clone(), ctx.clone());
    std::thread::Builder::new()
        .name("tiny-link".into())
        .spawn(move || match get_me(&probe) {
            Ok(me) => {
                let _ = probe_ctx.describe(&me.display_name, &me.email);
                probe_ctx.link(Link::Connected);
            }
            Err(Fault::Unauthorized) => probe_ctx.link(Link::Expired),
            Err(_) => {
                eprintln!(
                    "chatd: {}: could not reach the server",
                    probe_ctx.account.network.label()
                );
                probe_ctx.link(Link::Offline);
            }
        })
        .map_err(|err| err.to_string())?;
    std::thread::Builder::new()
        .name("tiny-commands".into())
        .spawn(move || {
            serve_commands(
                &ctx,
                inbox,
                Duration::from_millis(200),
                |_| {},
                |command| match command {
                    Command::SignOut(reply) => {
                        delete_session(&saved);
                        let _ = reply.send(Ok(()));
                    }
                    other => other.unsupported(&ctx),
                },
            );
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> &'static str {
        r#"{"base_url":"http://127.0.0.1:8080","token":"t","user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner"}"#
    }

    #[test]
    fn a_session_is_one_account_and_a_bad_one_is_an_error() {
        let signed = accounts(session()).unwrap();
        assert_eq!(signed.len(), 1);
        assert_eq!(signed[0].account, "11111111-1111-1111-1111-111111111111");
        assert_eq!(signed[0].name, "Ada");
        assert_eq!(signed[0].session, session());
        assert!(without(session(), &signed[0].account).is_none());
        assert_eq!(
            without(session(), "someone-else").as_deref(),
            Some(session())
        );
        assert!(accounts("not json").is_err());
        assert!(accounts(r#"{"base_url":"http://127.0.0.1:8080","token":"t","user_id":"../x","email":"a@b.co","display_name":"A","role":"owner"}"#).is_err());
        let saved = parse(session()).unwrap();
        assert_eq!(saved.role, "owner");
        assert_eq!(saved.base_url, "http://127.0.0.1:8080");
        assert_eq!(saved.email, "ada@example.com");
    }

    /// Speaks the session routes on a local socket: wrong password, open, me, delete, and a
    /// redirect that must not be followed.
    #[test]
    fn session_routes_against_a_local_server() {
        let base = serve(|request| {
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("POST /v1/session ") {
                if request.contains("\"password\": \"slow\"") {
                    return http(429, "Too Many Requests", r#"{"error":"slow_down"}"#);
                }
                if request.contains("\"password\": \"boom\"") {
                    return http(500, "Internal Server Error", r#"{"error":"unavailable"}"#);
                }
                if request.contains("\"password\": \"nope\"") {
                    return http(401, "Unauthorized", r#"{"error":"invalid_credentials"}"#);
                }
                return http(
                    200,
                    "OK",
                    r#"{"token":"tok_value","user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner"}"#,
                );
            }
            if first.starts_with("GET /v1/me ") {
                if request.contains("Bearer tok_value") {
                    return http(
                        200,
                        "OK",
                        r#"{"user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner"}"#,
                    );
                }
                return http(401, "Unauthorized", r#"{"error":"unauthorized"}"#);
            }
            if first.starts_with("DELETE /v1/session ") {
                return http(204, "No Content", "");
            }
            if first.starts_with("POST /v1/bounce ") {
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            } else {
                http(404, "Not Found", "{}")
            }
        });

        assert!(matches!(
            post_session(&base, "ada@example.com", "nope"),
            Err(Fault::Unauthorized)
        ));
        assert!(matches!(
            post_session(&base, "ada@example.com", "slow"),
            Err(Fault::SlowDown)
        ));
        assert!(matches!(
            post_session(&base, "ada@example.com", "boom"),
            Err(Fault::Unavailable)
        ));
        let saved = match post_session(&base, "ada@example.com", "correct-password") {
            Ok(saved) => saved,
            Err(_) => panic!("open failed"),
        };
        assert_eq!(saved.user_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(saved.role, "owner");
        assert_eq!(saved.email, "ada@example.com");
        assert_eq!(saved.base_url, base);
        assert_eq!(saved.token, "tok_value");

        let me = match get_me(&saved) {
            Ok(me) => me,
            Err(_) => panic!("me failed"),
        };
        assert_eq!(me.display_name, "Ada");
        assert_eq!(me.email, "ada@example.com");
        delete_session(&saved);

        let bounced = call(
            "POST",
            &endpoint(&base, "/v1/bounce"),
            None,
            Some(serde_json::json!({"email": "ada@example.com", "password": "correct-password"})),
        );
        assert!(matches!(bounced, Err(Fault::Unreachable)));
    }

    /// Against a tinyserver that is already serving. Not part of `cargo test`: it needs
    /// `TINY_LIVE_BASE`, `TINY_LIVE_EMAIL`, and `TINY_LIVE_PASSWORD`.
    #[test]
    #[ignore]
    fn live_owner_round_trip() {
        let base = std::env::var("TINY_LIVE_BASE").expect("TINY_LIVE_BASE");
        let email = std::env::var("TINY_LIVE_EMAIL").expect("TINY_LIVE_EMAIL");
        let password = std::env::var("TINY_LIVE_PASSWORD").expect("TINY_LIVE_PASSWORD");
        let saved = match post_session(&base, &email, &password) {
            Ok(saved) => saved,
            Err(_) => panic!("open failed"),
        };
        assert_eq!(saved.role, "owner");
        assert_eq!(saved.email, email.trim().to_ascii_lowercase());
        let me = match get_me(&saved) {
            Ok(me) => me,
            Err(_) => panic!("me failed"),
        };
        assert_eq!(me.email, saved.email);
        delete_session(&saved);
        assert!(matches!(get_me(&saved), Err(Fault::Unauthorized)));
    }

    fn http(status: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn serve(handler: impl Fn(&str) -> Vec<u8> + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            listener.set_nonblocking(true).expect("nonblocking");
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                        let mut got = Vec::new();
                        let mut buf = [0u8; 2048];
                        while got.windows(4).all(|window| window != b"\r\n\r\n") {
                            match std::io::Read::read(&mut stream, &mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(read) => got.extend_from_slice(&buf[..read]),
                            }
                        }
                        let request = String::from_utf8_lossy(&got).into_owned();
                        let _ = std::io::Write::write_all(&mut stream, &handler(&request));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        format!("http://127.0.0.1:{port}")
    }
}
