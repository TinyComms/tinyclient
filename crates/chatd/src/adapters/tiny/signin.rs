//! Signing in to a Tiny server the owner already runs: the address, then the email, then the
//! password. Nothing is kept until `POST /v1/session` answers 200. The password and the bearer
//! are not logged. One sign-in at a time, in memory; closing the sheet drops it.

use super::{post_session, Fault};
use crate::hub::Hub;
use protocol::{AccountKey, Network, SignInInput, SignInProblem, SignInStep};
use std::sync::{Arc, Mutex, MutexGuard};

/// Keeps a finished session and starts its account.
pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

struct Flow {
    server: String,
    email: String,
    step: SignInStep,
    adopt: Adopt,
}

static FLOW: Mutex<Option<Flow>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Flow>> {
    FLOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An http or https origin: no user, no query, no fragment, no path. A trailing slash is dropped.
/// A host with no scheme is https, except a machine on this computer, which is http.
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
    if rest.contains(['@', '?', '#', '\\']) {
        return None;
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() || rest.contains('/') {
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

/// One `@`, something on each side, and no whitespace.
pub(crate) fn email_ok(raw: &str) -> bool {
    let email = raw.trim();
    if email.is_empty() || email.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty() && !domain.is_empty() && !domain.contains('@')
}

/// Begins a sign-in, or with `input` answers the step on show.
pub fn step(
    _hub: &Arc<Hub>,
    input: Option<SignInInput>,
    adopt: Adopt,
) -> Result<SignInStep, String> {
    let mut held = current();
    let Some(input) = input else {
        if held.is_none() {
            *held = Some(Flow {
                server: String::new(),
                email: String::new(),
                step: SignInStep::Server { problem: None },
                adopt,
            });
        }
        return Ok(held
            .as_ref()
            .map(|flow| flow.step.clone())
            .unwrap_or(SignInStep::Server { problem: None }));
    };
    let Some(flow) = held.as_mut() else {
        return Ok(SignInStep::Failed {
            reason: "That sign-in is over. Start again.".to_owned(),
        });
    };
    let next = flow.answer(input);
    if matches!(next, SignInStep::Done { .. } | SignInStep::Failed { .. }) {
        *held = None;
    }
    Ok(next)
}

/// The user closed the sheet. Nothing was kept.
pub fn cancel() {
    *current() = None;
}

impl Flow {
    fn answer(&mut self, input: SignInInput) -> SignInStep {
        let next = match (&self.step, input) {
            (SignInStep::Server { .. }, SignInInput::Server { url }) => match base_url(&url) {
                Some(server) => {
                    self.server = server.clone();
                    SignInStep::Email {
                        server,
                        problem: None,
                    }
                }
                None => SignInStep::Server {
                    problem: Some(SignInProblem::UrlInvalid),
                },
            },
            (SignInStep::Email { .. }, SignInInput::Email { email }) => {
                if email_ok(&email) {
                    self.email = email.trim().to_owned();
                    SignInStep::Password {
                        hint: String::new(),
                        problem: None,
                    }
                } else {
                    SignInStep::Email {
                        server: self.server.clone(),
                        problem: Some(SignInProblem::EmailInvalid),
                    }
                }
            }
            (SignInStep::Email { .. }, SignInInput::Back) => SignInStep::Server { problem: None },
            (SignInStep::Password { .. }, SignInInput::Back) => SignInStep::Email {
                server: self.server.clone(),
                problem: None,
            },
            (SignInStep::Password { .. }, SignInInput::Password { password }) => {
                self.submit(&password)
            }
            _ => self.step.clone(),
        };
        self.step = next.clone();
        next
    }

    fn submit(&self, password: &str) -> SignInStep {
        let again = |problem| SignInStep::Password {
            hint: String::new(),
            problem: Some(problem),
        };
        match post_session(&self.server, &self.email, password) {
            Ok(saved) => {
                let account = saved.user_id.clone();
                let session = match serde_json::to_string(&saved) {
                    Ok(session) => session,
                    Err(_) => {
                        return SignInStep::Failed {
                            reason: "The server's answer could not be kept.".to_owned(),
                        }
                    }
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
            Err(Fault::Unauthorized) => again(SignInProblem::PasswordInvalid),
            Err(Fault::Unreachable) => again(SignInProblem::Unreachable),
            Err(Fault::BadAnswer) => SignInStep::Failed {
                reason: "The server's answer could not be read.".to_owned(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn hub() -> Arc<Hub> {
        Arc::new(Hub::new(
            Store::open(std::path::Path::new(":memory:")).expect("a store in memory"),
        ))
    }

    fn adopt() -> Adopt {
        Box::new(|_session| Ok(Vec::new()))
    }

    #[test]
    fn a_bad_address_stays_on_the_server_step_and_a_bad_email_stays_on_email() {
        cancel();
        let hub = hub();
        assert_eq!(
            step(&hub, None, adopt()).unwrap(),
            SignInStep::Server { problem: None }
        );
        assert_eq!(
            step(
                &hub,
                Some(SignInInput::Server {
                    url: "not a url".into()
                }),
                adopt()
            )
            .unwrap(),
            SignInStep::Server {
                problem: Some(SignInProblem::UrlInvalid)
            }
        );
        assert_eq!(
            step(
                &hub,
                Some(SignInInput::Server {
                    url: "http://user:pw@127.0.0.1:8080".into()
                }),
                adopt()
            )
            .unwrap(),
            SignInStep::Server {
                problem: Some(SignInProblem::UrlInvalid)
            }
        );
        assert_eq!(
            step(
                &hub,
                Some(SignInInput::Server {
                    url: "http://127.0.0.1:8080/v1".into()
                }),
                adopt()
            )
            .unwrap(),
            SignInStep::Server {
                problem: Some(SignInProblem::UrlInvalid)
            }
        );
        assert_eq!(
            step(
                &hub,
                Some(SignInInput::Server {
                    url: "http://127.0.0.1:8080/".into()
                }),
                adopt()
            )
            .unwrap(),
            SignInStep::Email {
                server: "http://127.0.0.1:8080".into(),
                problem: None
            }
        );
        assert_eq!(
            step(
                &hub,
                Some(SignInInput::Email {
                    email: "not an email".into()
                }),
                adopt()
            )
            .unwrap(),
            SignInStep::Email {
                server: "http://127.0.0.1:8080".into(),
                problem: Some(SignInProblem::EmailInvalid)
            }
        );
        assert_eq!(
            step(
                &hub,
                Some(SignInInput::Email {
                    email: "ada@example.com".into()
                }),
                adopt()
            )
            .unwrap(),
            SignInStep::Password {
                hint: String::new(),
                problem: None
            }
        );
        assert_eq!(
            step(&hub, Some(SignInInput::Back), adopt()).unwrap(),
            SignInStep::Email {
                server: "http://127.0.0.1:8080".into(),
                problem: None
            }
        );
        assert_eq!(
            step(&hub, Some(SignInInput::Back), adopt()).unwrap(),
            SignInStep::Server { problem: None }
        );
        cancel();
    }

    #[test]
    fn the_address_is_an_origin_and_the_email_has_one_at() {
        assert_eq!(
            base_url("  https://Example.com/  ").as_deref(),
            Some("https://Example.com")
        );
        assert_eq!(
            base_url("private.tinyclient.io").as_deref(),
            Some("https://private.tinyclient.io")
        );
        assert_eq!(
            base_url("127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(base_url("localhost").as_deref(), Some("http://localhost"));
        assert_eq!(
            base_url("http://[::1]:8080").as_deref(),
            Some("http://[::1]:8080")
        );
        assert_eq!(base_url("http://127.0.0.1:8080?x=1"), None);
        assert_eq!(base_url("http://127.0.0.1:8080#top"), None);
        assert_eq!(base_url("ftp://127.0.0.1:8080"), None);
        assert_eq!(base_url("http://127.0.0.1:0"), None);
        assert!(email_ok("ada@example.com"));
        assert!(email_ok(" a@b "));
        assert!(!email_ok("ada"));
        assert!(!email_ok("a@b@c"));
        assert!(!email_ok("a @b.co"));
        assert!(!email_ok("@b.co"));
        assert!(!format!(
            "{:?}",
            SignInInput::Server {
                url: "http://secret.example".into()
            }
        )
        .contains("secret"));
        assert!(!format!(
            "{:?}",
            SignInInput::Email {
                email: "ada@example.com".into()
            }
        )
        .contains("ada"));
        assert!(!format!(
            "{:?}",
            SignInInput::Password {
                password: "hunter2".into()
            }
        )
        .contains("hunter2"));
    }
}
