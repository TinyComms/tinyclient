//! Tinyserver application bots. An owner or an admin creates, lists and revokes them with their
//! session (`Authorization: Bearer`). The token is shown once, on create. The bot calls the API
//! as itself (`Authorization: Bot`), which is not a session.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use ureq::tls::{TlsConfig, TlsProvider};

/// How long one request may take. A redirect is not followed, so this is one host.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Why a bot call did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// 401 or 403. A member listing bots is this.
    Denied,
    /// The host could not be asked, or answered with a redirect. The token stays here.
    Unreachable,
    /// The status was a success but the body was not the shape this call returns.
    BadAnswer,
    /// Any other status.
    Rejected(u16),
}

/// A bot as `GET /v1/bots` lists it. The token is not in this answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Bot {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: String,
}

/// A bot the server just created. `token` is shown this once.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreatedBot {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub token: String,
    pub created_at: String,
}

/// `GET /v1/me` answered with a bot token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BotMe {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub scopes: Vec<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
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

fn endpoint(base: &str, path: &str) -> String {
    format!("{base}{path}")
}

/// One request. `authorization` is the whole header value: `Bearer <session>` or `Bot <token>`.
/// A 3xx is a failure, so neither token is followed off the host that was asked.
fn call(
    method: &str,
    url: &str,
    authorization: &str,
    body: Option<Value>,
) -> Result<(u16, Value), Fault> {
    let agent = agent();
    let sent = match method {
        "GET" => agent.get(url).header("Authorization", authorization).call(),
        "DELETE" => agent
            .delete(url)
            .header("Authorization", authorization)
            .call(),
        "POST" => {
            let request = agent.post(url).header("Authorization", authorization);
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
    // The body is not logged: a 201 holds the bot token.
    let parsed = response.body_mut().read_json().unwrap_or(Value::Null);
    Ok((status, parsed))
}

fn bearer(session_token: &str) -> String {
    format!("Bearer {session_token}")
}

fn bot_authorization(bot_token: &str) -> String {
    format!("Bot {bot_token}")
}

fn expect_status(status: u16, want: u16) -> Result<(), Fault> {
    if status == want {
        return Ok(());
    }
    if status == 401 || status == 403 {
        return Err(Fault::Denied);
    }
    Err(Fault::Rejected(status))
}

fn parse<T: DeserializeOwned>(body: Value) -> Result<T, Fault> {
    serde_json::from_value(body).map_err(|_| Fault::BadAnswer)
}

/// Creates a bot. `session_token` is an owner or admin session, sent as `Bearer`.
pub fn create_bot(
    base_url: &str,
    session_token: &str,
    name: &str,
    scopes: &[&str],
) -> Result<CreatedBot, Fault> {
    let (status, body) = call(
        "POST",
        &endpoint(base_url, "/v1/bots"),
        &bearer(session_token),
        Some(serde_json::json!({ "name": name, "scopes": scopes })),
    )?;
    expect_status(status, 201)?;
    parse(body)
}

/// Lists bots. Sent as `Bearer`. A member is [`Fault::Denied`]. The rows have no token.
pub fn list_bots(base_url: &str, session_token: &str) -> Result<Vec<Bot>, Fault> {
    let (status, body) = call(
        "GET",
        &endpoint(base_url, "/v1/bots"),
        &bearer(session_token),
        None,
    )?;
    expect_status(status, 200)?;
    parse(body)
}

/// Revokes a bot. Sent as `Bearer`. The server answers 204.
pub fn revoke_bot(base_url: &str, session_token: &str, id: &str) -> Result<(), Fault> {
    let (status, _) = call(
        "DELETE",
        &endpoint(base_url, &format!("/v1/bots/{id}")),
        &bearer(session_token),
        None,
    )?;
    expect_status(status, 204)
}

/// Who this bot is. Sent as `Bot <bot_token>`, not as a session.
pub fn bot_me(base_url: &str, bot_token: &str) -> Result<BotMe, Fault> {
    let (status, body) = call(
        "GET",
        &endpoint(base_url, "/v1/me"),
        &bot_authorization(bot_token),
        None,
    )?;
    expect_status(status, 200)?;
    parse(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn authorization(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default()
    }

    fn json_body(request: &str) -> Value {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|err| panic!("body {err}: {body}"))
    }

    fn http(status: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn read_some(stream: &mut TcpStream, got: &mut Vec<u8>, deadline: Instant) -> bool {
        let mut buf = [0u8; 2048];
        while Instant::now() < deadline {
            match stream.read(&mut buf) {
                Ok(0) => return false,
                Err(err)
                    if err.kind() == std::io::ErrorKind::TimedOut
                        || err.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    continue;
                }
                Err(_) => return false,
                Ok(read) => {
                    got.extend_from_slice(&buf[..read]);
                    return true;
                }
            }
        }
        false
    }

    /// One local request. The closure sees the raw request, headers included.
    fn serve(handler: impl Fn(&str) -> Vec<u8> + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            listener.set_nonblocking(true).expect("nonblocking");
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                        let mut got = Vec::new();
                        let header_end = loop {
                            if let Some(at) =
                                got.windows(4).position(|window| window == b"\r\n\r\n")
                            {
                                break Some(at + 4);
                            }
                            if !read_some(&mut stream, &mut got, deadline) {
                                break None;
                            }
                        };
                        if let Some(header_end) = header_end {
                            let head = String::from_utf8_lossy(&got[..header_end]);
                            let length = head.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            });
                            if let Some(length) = length {
                                while got.len() < header_end + length
                                    && read_some(&mut stream, &mut got, deadline)
                                {
                                }
                            }
                        }
                        let request = String::from_utf8_lossy(&got).into_owned();
                        let _ = stream.write_all(&handler(&request));
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

    #[test]
    fn create_bot_sends_bearer_and_posts_the_name_and_scopes() {
        let seen = Arc::new(Mutex::new(String::new()));
        let keep = Arc::clone(&seen);
        let base = serve(move |request| {
            *keep.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = request.to_owned();
            http(
                201,
                "Created",
                r#"{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","name":"Herald","scopes":["rooms.read"],"token":"shown-once","created_at":"2026-10-30T00:00:00Z"}"#,
            )
        });
        let created =
            create_bot(&base, "owner-session", "Herald", &["rooms.read"]).expect("created");
        let request = seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(request.starts_with("POST /v1/bots "), "{request}");
        assert_eq!(authorization(&request), "Bearer owner-session");
        assert_eq!(
            json_body(&request),
            serde_json::json!({"name": "Herald", "scopes": ["rooms.read"]})
        );
        assert_eq!(
            created,
            CreatedBot {
                id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_owned(),
                name: "Herald".to_owned(),
                scopes: vec!["rooms.read".to_owned()],
                token: "shown-once".to_owned(),
                created_at: "2026-10-30T00:00:00Z".to_owned(),
            }
        );
    }

    #[test]
    fn list_bots_sends_bearer_and_a_member_is_forbidden() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&seen);
        let base = serve(move |request| {
            keep.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.to_owned());
            if request.contains("Bearer member-session") {
                return http(403, "Forbidden", r#"{"error":"forbidden"}"#);
            }
            http(
                200,
                "OK",
                r#"[{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","name":"Herald","scopes":["rooms.read","members.read"],"created_at":"2026-10-30T00:00:00Z"}]"#,
            )
        });
        let listed = list_bots(&base, "owner-session").expect("listed");
        assert_eq!(list_bots(&base, "member-session"), Err(Fault::Denied));
        let requests = seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /v1/bots "), "{}", requests[0]);
        assert_eq!(authorization(&requests[0]), "Bearer owner-session");
        assert!(requests[1].starts_with("GET /v1/bots "), "{}", requests[1]);
        assert_eq!(authorization(&requests[1]), "Bearer member-session");
        assert_eq!(
            listed,
            vec![Bot {
                id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_owned(),
                name: "Herald".to_owned(),
                scopes: vec!["rooms.read".to_owned(), "members.read".to_owned()],
                created_at: "2026-10-30T00:00:00Z".to_owned(),
            }]
        );
    }

    #[test]
    fn revoke_bot_sends_bearer_and_accepts_no_content() {
        let seen = Arc::new(Mutex::new(String::new()));
        let keep = Arc::clone(&seen);
        let base = serve(move |request| {
            *keep.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = request.to_owned();
            http(204, "No Content", "")
        });
        let id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        revoke_bot(&base, "owner-session", id).expect("revoked");
        let request = seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(
            request.starts_with(&format!("DELETE /v1/bots/{id} ")),
            "{request}"
        );
        assert_eq!(authorization(&request), "Bearer owner-session");
    }

    #[test]
    fn bot_me_sends_the_bot_prefix() {
        let seen = Arc::new(Mutex::new(String::new()));
        let keep = Arc::clone(&seen);
        let base = serve(move |request| {
            *keep.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = request.to_owned();
            http(
                200,
                "OK",
                r#"{"user_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","email":null,"display_name":"Herald","role":"bot","scopes":["rooms.read"]}"#,
            )
        });
        let me = bot_me(&base, "bot-token").expect("me");
        let request = seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(request.starts_with("GET /v1/me "), "{request}");
        assert_eq!(authorization(&request), "Bot bot-token");
        assert_eq!(
            me,
            BotMe {
                user_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_owned(),
                display_name: "Herald".to_owned(),
                role: "bot".to_owned(),
                scopes: vec!["rooms.read".to_owned()],
            }
        );
    }
}
