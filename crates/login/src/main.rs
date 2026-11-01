//! `tinyclient-login <network>`: the one place a web page is shown.
//!
//! Slack and Discord sign-in (passwords, SSO, 2FA, captchas) only exists as their web pages, so
//! this opens that page in a window, waits for the signed-in session to appear, prints it as one
//! line of JSON on stdout, and exits. The browser itself is kept, so closing the window or
//! starting again resumes the same Slack or Discord sign-in instead of beginning from the email
//! field. chatd stores the finished session in the login keychain.
//!
//! The session says which browser signed in (`user_agent`), so chatd can go on being that
//! browser. A Slack session lists every workspace the browser holds: `teams`, each with its own
//! token, beside the one `cookie` they share.
//!
//! `--at X,Y` centres the window on that point, given in points from the top left of the main
//! display: the middle of the window that asked for the sign-in.
//!
//! `messages` uses an installed Chrome, Edge or Chromium with a disposable private profile.
//! It prints `{cookies:{...},user_agent:...}` after Google's account page reaches Messages'
//! config endpoint, then destroys the profile. It accepts `--at` but the external browser
//! chooses its own placement. No existing browser profile is opened or read.
//!
//! Exit status: 0 with the session; 1 the user closed the window; 3 the page could not be
//! loaded; 4 the user closed a window showing that their organization blocks this sign-in;
//! 5 Slack's web client opened signed in but the browser holds no token to read; 64 bad
//! arguments.
//! CONTRACT: chatd's `sign_in` reads every failure as "sign-in was cancelled" and passes no
//! `--at`; both are there for it to use.

mod common;
mod teams;
mod messages;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod desktop;

pub(crate) use common::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("tinyclient-login {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let _log = protocol::log_policy::install("login", protocol::log_policy::LogPolicy::from_env());
    protocol::crash::install_panic_hook("login");
    if args.first().map(String::as_str) == Some("--messages-browser-cleanup") {
        messages::cleanup_worker(&args);
        return;
    }
    let network = match args.first().map(String::as_str) {
        Some("slack") => "slack",
        Some("discord") => "discord",
        Some("teams") => "teams",
        Some("messages") => "messages",
        other => {
            tracing::warn!("tinyclient-login: no web sign-in for {other:?}");
            std::process::exit(USAGE);
        }
    };
    if network == "messages" {
        if let Err(why) = parse_at(&args) {
            tracing::warn!("tinyclient-login: {why}");
            std::process::exit(USAGE);
        }
        messages::run();
    }
    let site = match network {
        "slack" => SLACK,
        "discord" => DISCORD,
        "teams" => teams::site(),
        _ => unreachable!(),
    };
    let at = parse_at(&args).unwrap_or_else(|why| {
        tracing::warn!("tinyclient-login: {why}");
        std::process::exit(USAGE);
    });
    #[cfg(target_os = "macos")]
    macos::run(site, at);
    #[cfg(not(target_os = "macos"))]
    desktop::run(site, at, network);
}
