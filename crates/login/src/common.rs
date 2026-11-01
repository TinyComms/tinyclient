//! Shared sign-in logic for every platform.

use std::io::Write;

pub const CLOSED: i32 = 1;
pub const LOAD_FAILED: i32 = 3;
pub const BLOCKED: i32 = 4;
pub const NO_TOKEN: i32 = 5;
pub const USAGE: i32 = 64;
pub const SIZE: (f64, f64) = (520., 760.);

/// How long Slack's web client may stay open with no token to read before this gives up.
pub const NO_TOKEN_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Copy)]
pub struct Site {
    pub title: &'static str,
    pub start: &'static str,
    pub probe: &'static str,
    pub read: fn(&str) -> Option<serde_json::Value>,
    pub cookie: Option<&'static str>,
    pub detour: Option<(&'static str, &'static str)>,
}

pub const SLACK: Site = Site {
    title: "Sign in to Slack",
    start: "https://slack.com/signin#/signin",
    probe: "(()=>{try{return localStorage.getItem('localConfig_v2')}catch(e){return null}})()",
    read: slack_session,
    cookie: Some("d"),
    detour: Some(("/ssb/redirect", "https://app.slack.com/client")),
};

pub const DISCORD: Site = Site {
    title: "Sign in to Discord",
    start: "https://discord.com/login",
    // The client may remove localStorage from its own window. A same-origin
    // frame still exposes the shared storage. hands_off() prevents this probe
    // from running while the user is typing on Discord's login page.
    probe: include_str!("discord-probe.js"),
    read: |found| serde_json::from_str(found).ok(),
    cookie: None,
    detour: None,
};

/// Discord's email, password, 2FA and captcha all stay on this address. Script run against it
/// (every second, while the person is typing) blurs the password field, so they have to enter
/// it again and again. The token is read once the page has left this address.
pub fn hands_off(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.strip_prefix("https://discord.com/login")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

pub const PAGE: &str = "JSON.stringify({url:location.href,\
                    text:document.body?document.body.innerText.slice(0,4000):''})";

pub fn user_agent() -> String {
    protocol::browser::user_agent()
}

pub fn slack_session(local_config: &str) -> Option<serde_json::Value> {
    let config: serde_json::Value = serde_json::from_str(local_config).ok()?;
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_owned();
    let teams: Vec<serde_json::Value> = config["teams"]
        .as_object()?
        .iter()
        .filter(|(_, team)| !text(&team["token"]).is_empty() && !text(&team["url"]).is_empty())
        .map(|(id, team)| {
            serde_json::json!({
                "id": id,
                "name": text(&team["name"]),
                "url": text(&team["url"]),
                "token": text(&team["token"]),
                "enterprise_id": text(&team["enterprise_id"]),
            })
        })
        .collect();
    let first = text(&teams.first()?["id"]);
    let active = match config["lastActiveTeamId"].as_str() {
        Some(active) if !active.is_empty() => active.to_owned(),
        _ => first,
    };
    Some(serde_json::json!({"teams": teams, "active": active}))
}

pub fn web_client(url: &str) -> bool {
    url.strip_prefix("https://app.slack.com/client")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

#[derive(Debug, PartialEq)]
pub enum Route {
    Allow,
    Rewrite(String),
    Block,
}

pub fn route(url: &str, from: Option<&str>) -> Route {
    let app = url.get(..6).is_some_and(|scheme| scheme.eq_ignore_ascii_case("slack:"));
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if !(app || path.contains("/ssb/redirect")) {
        return Route::Allow;
    }
    if app && from.is_some_and(web_client) {
        return Route::Block;
    }
    Route::Rewrite(match team_id(url) {
        Some(team) => format!("https://app.slack.com/client/{team}"),
        None => "https://app.slack.com/client".to_owned(),
    })
}

pub fn team_id(url: &str) -> Option<&str> {
    let named = url.split_once('?').and_then(|(_, query)| {
        let query = query.split('#').next()?;
        query.split('&').find_map(|pair| pair.strip_prefix("team="))
    });
    let host = url.get(8..).filter(|_| url[..8].eq_ignore_ascii_case("slack://"));
    let id = named.or_else(|| host?.split(['/', '?', '#']).next())?;
    let ok = id.len() > 1
        && id.starts_with(['T', 'E'])
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    ok.then_some(id)
}

pub fn blocked(url: &str, text: &str) -> bool {
    const ENTRA: [&str; 6] = [
        "AADSTS53000",
        "AADSTS53001",
        "AADSTS53002",
        "AADSTS53003",
        "AADSTS530002",
        "AADSTS530003",
    ];
    const WORDS: [&str; 6] = [
        "you can't get there from here",
        "you cannot access this right now",
        "isn't allowed by your organization",
        "not allowed by your organization",
        "requires an approved device",
        "your organization requires",
    ];
    let text = text.to_lowercase().replace('\u{2019}', "'");
    ENTRA
        .iter()
        .any(|code| text.contains(&code.to_lowercase()) || url.contains(code))
        || WORDS.iter().any(|words| text.contains(words))
}

#[cfg(target_os = "macos")]
pub fn load_failed(domain: &str, code: isize) -> bool {
    domain == "NSURLErrorDomain"
        && matches!(code, -1001 | -1003 | -1004 | -1005 | -1006 | -1009 | -1206..=-1200)
}

/// True when a navigation error string looks like the page never arrived.
#[cfg(not(target_os = "macos"))]
pub fn load_failed_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "err_connection",
        "err_name_not_resolved",
        "err_internet_disconnected",
        "err_connection_timed_out",
        "err_connection_refused",
        "err_connection_reset",
        "err_ssl",
        "err_cert",
        "cannot resolve",
        "could not connect",
        "network error",
        "failed to load",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub fn parse_at(args: &[String]) -> Result<Option<(f64, f64)>, String> {
    let Some(flag) = args.iter().position(|arg| arg == "--at") else {
        return Ok(None);
    };
    let value = args.get(flag + 1).ok_or("--at needs X,Y")?;
    let point = value
        .split_once(',')
        .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)));
    match point {
        Some((x, y)) if f64::is_finite(x) && f64::is_finite(y) => Ok(Some((x, y))),
        _ => Err(format!("--at needs X,Y, not {value:?}")),
    }
}

type Rect = (f64, f64, f64, f64);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn origin(at: (f64, f64), size: (f64, f64), main_height: f64, visible: Option<Rect>) -> (f64, f64) {
    let (mut x, mut y) = (at.0 - size.0 / 2., main_height - at.1 - size.1 / 2.);
    if let Some((left, bottom, width, height)) = visible {
        x = x.min(left + width - size.0).max(left);
        y = y.min(bottom + height - size.1).max(bottom);
    }
    (x, y)
}

pub fn session(mut probed: serde_json::Value, cookie: Option<String>, agent: &str) -> serde_json::Value {
    if let Some(cookie) = cookie {
        probed["cookie"] = cookie.into();
    }
    probed["user_agent"] = agent.into();
    probed
}

pub fn finish(session: serde_json::Value) -> ! {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{session}");
    let _ = out.flush();
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::{
        blocked, hands_off, origin, parse_at, route, session, slack_session, web_client, Route, DISCORD,
    };

    #[test]
    fn a_slack_session_lists_every_workspace_with_the_cookie_and_the_browser() {
        let probed = serde_json::json!({
            "teams": [
                {"id": "T1", "name": "Acme", "url": "https://acme.slack.com/", "token": "xoxc-1"},
                {"id": "T2", "name": "Side", "url": "https://side.slack.com/", "token": "xoxc-2"},
            ],
            "active": "T2",
        });
        let made = session(probed, Some("xoxd-abc".into()), "Safari/26.0");
        assert_eq!(made["teams"].as_array().map(Vec::len), Some(2));
        assert_eq!(made["teams"][1]["token"], "xoxc-2");
        assert_eq!(made["cookie"], "xoxd-abc");
        assert_eq!(made["user_agent"], "Safari/26.0");
        let discord = session(serde_json::json!({"token": "abc"}), None, "Safari/26.0");
        assert_eq!(discord, serde_json::json!({"token": "abc", "user_agent": "Safari/26.0"}));
    }

    #[test]
    fn at_takes_a_point_and_refuses_anything_else() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(parse_at(&args(&["slack"])), Ok(None));
        assert_eq!(parse_at(&args(&["slack", "--at", "640,400.5"])), Ok(Some((640., 400.5))));
        assert_eq!(parse_at(&args(&["slack", "--at", "-200, 90"])), Ok(Some((-200., 90.))));
        assert!(parse_at(&args(&["slack", "--at"])).is_err());
        assert!(parse_at(&args(&["slack", "--at", "640"])).is_err());
        assert!(parse_at(&args(&["slack", "--at", "a,b"])).is_err());
        assert!(parse_at(&args(&["slack", "--at", "nan,1"])).is_err());
    }

    #[test]
    fn the_window_is_centred_on_the_point_and_kept_on_its_screen() {
        let visible = Some((0., 0., 1440., 875.));
        assert_eq!(origin((720., 450.), (520., 760.), 900., visible), (460., 70.));
        assert_eq!(origin((10., 10.), (520., 760.), 900., visible), (0., 115.));
        assert_eq!(origin((1430., 890.), (520., 760.), 900., visible), (920., 0.));
        assert_eq!(origin((2000., 100.), (520., 760.), 900., None), (1740., 420.));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_page_that_never_arrived_is_a_load_failure() {
        use super::load_failed;
        assert!(load_failed("NSURLErrorDomain", -1009));
        assert!(load_failed("NSURLErrorDomain", -1003));
        assert!(load_failed("NSURLErrorDomain", -1202));
        assert!(!load_failed("NSURLErrorDomain", -999));
        assert!(!load_failed("NSURLErrorDomain", -1002));
        assert!(!load_failed("WebKitErrorDomain", 102));
    }

    #[test]
    fn an_organizations_refusal_is_told_from_an_ordinary_page() {
        let entra = "You can\u{2019}t get there from here\nThis application contains sensitive information";
        assert!(blocked("https://login.microsoftonline.com/common/login", entra));
        assert!(blocked("https://login.microsoftonline.com/x", "Error code: AADSTS53003"));
        assert!(blocked("https://acme.slack.com/", "Signing in from this browser isn't allowed by your organization."));
        assert!(!blocked("https://slack.com/signin", "Sign in to Slack\nWe suggest using the email address you use at work."));
        assert!(!blocked("https://app.slack.com/client/T1", ""));
    }

    fn to(url: &str) -> Route {
        Route::Rewrite(url.to_owned())
    }

    #[test]
    fn workspace_arrow_opens_that_workspace() {
        let signin = Some("https://slack.com/signin");
        let url = "https://app.slack.com/ssb/redirect?entry_point=workspace_signin&team=T0G1MMR";
        assert_eq!(route(url, signin), to("https://app.slack.com/client/T0G1MMR"));
        let url = "https://app.slack.com/ssb/redirect?team=E01ABCDEF&entry_point=signin#/";
        assert_eq!(route(url, signin), to("https://app.slack.com/client/E01ABCDEF"));
    }

    #[test]
    fn welcome_back_launch_stays_in_the_window_and_ends_on_the_web_client() {
        let welcome = Some("https://slack.com/signin#/workspaces");
        let launch = "https://acme.slack.com/ssb/redirect?entry_point=workspace_signin";
        assert_eq!(route(launch, welcome), to("https://app.slack.com/client"));
        assert_eq!(route("https://acme.slack.com/ssb/redirect", None), to("https://app.slack.com/client"));
        let redirect = Some(launch);
        let magic = "slack://T0G1MMR/magic-login/3564479012256-0a1b2c3d4e5f";
        assert_eq!(route(magic, redirect), to("https://app.slack.com/client/T0G1MMR"));
        assert_eq!(route("slack://open?team=TPREMIER", redirect), to("https://app.slack.com/client/TPREMIER"));
        assert_eq!(route("slack://channel?id=C024BE91L&team=T0G1MMR", redirect), to("https://app.slack.com/client/T0G1MMR"));
        assert_eq!(route("slack://open", redirect), to("https://app.slack.com/client"));
        assert_eq!(route("SLACK://open?team=not-a-team", None), to("https://app.slack.com/client"));
        assert_eq!(route("https://app.slack.com/client/T0G1MMR", redirect), Route::Allow);
        assert_eq!(route("https://app.slack.com/client/T0G1MMR/C024BE91L", redirect), Route::Allow);
    }

    #[test]
    fn an_app_link_from_the_web_client_is_dropped_not_reloaded() {
        let client = Some("https://app.slack.com/client/T0G1MMR/C024BE91L");
        assert_eq!(route("slack://open?team=T0G1MMR", client), Route::Block);
        assert_eq!(route("slack://T0G1MMR/magic-login/1-a", Some("https://app.slack.com/client")), Route::Block);
        let url = "https://app.slack.com/ssb/redirect?team=T0G1MMR";
        assert_eq!(route(url, client), to("https://app.slack.com/client/T0G1MMR"));
    }

    #[test]
    fn ordinary_pages_stay_put() {
        assert_eq!(route("https://app.slack.com/client/T0G1MMR", None), Route::Allow);
        assert_eq!(route("https://glimmr.slack.com/", None), Route::Allow);
        assert_eq!(route("https://slack.com/signin#/signin", None), Route::Allow);
        assert_eq!(route("https://discord.com/login", None), Route::Allow);
        let url = "https://slack.com/checkcookie?redir=https://acme.slack.com/ssb/redirect";
        assert_eq!(route(url, None), Route::Allow);
        assert_eq!(route("https://slackers.example/x", None), Route::Allow);
    }

    #[test]
    fn the_web_client_is_told_from_the_pages_before_it() {
        assert!(web_client("https://app.slack.com/client"));
        assert!(web_client("https://app.slack.com/client/T0G1MMR/C024BE91L"));
        assert!(web_client("https://app.slack.com/client/E01ABCDEF?x=1"));
        assert!(!web_client("https://app.slack.com/client-boot"));
        assert!(!web_client("https://app.slack.com/ssb/redirect?team=T0G1MMR"));
        assert!(!web_client("https://slack.com/signin#/signin"));
        assert!(!web_client("https://acme.enterprise.slack.com/"));
        assert!(!web_client(""));
    }

    #[test]
    fn a_workspace_token_is_read_from_local_config() {
        let config = r#"{"teams":{"T0G1MMR":{"id":"T0G1MMR","name":"Acme","domain":"acme",
            "url":"https://acme.slack.com/","token":"xoxc-1","user_id":"U1"}},
            "lastActiveTeamId":"T0G1MMR","version":2}"#;
        let expected = serde_json::json!({
            "teams": [{"id": "T0G1MMR", "name": "Acme", "url": "https://acme.slack.com/",
                       "token": "xoxc-1", "enterprise_id": ""}],
            "active": "T0G1MMR",
        });
        assert_eq!(slack_session(config), Some(expected));
    }

    #[test]
    fn several_teams_each_keep_their_token_and_the_ones_without_are_left_out() {
        let config = r#"{"teams":{
            "T1":{"name":"Acme","url":"https://acme.slack.com/","token":"xoxc-1"},
            "T2":{"url":"https://side.slack.com/","token":"xoxc-2","enterprise_id":"E9"},
            "T3":{"name":"Signed out","url":"https://gone.slack.com/"},
            "T4":{"name":"No address","token":"xoxc-4"},
            "T5":null},
            "lastActiveTeamId":"T2"}"#;
        let made = slack_session(config).expect("a session");
        let teams = made["teams"].as_array().expect("teams");
        assert_eq!(teams.len(), 2);
        assert_eq!((&teams[0]["id"], &teams[0]["token"]), (&"T1".into(), &"xoxc-1".into()));
        assert_eq!(teams[1], serde_json::json!({"id": "T2", "name": "", "url": "https://side.slack.com/",
            "token": "xoxc-2", "enterprise_id": "E9"}));
        assert_eq!(made["active"], "T2");
        let config = r#"{"teams":{"T7":{"url":"https://a.slack.com/","token":"xoxc-7"}}}"#;
        assert_eq!(slack_session(config).expect("a session")["active"], "T7");
    }

    #[test]
    fn a_grid_organization_with_no_token_is_no_session() {
        assert_eq!(slack_session(r#"{"teams":{},"lastActiveTeamId":"T1","version":2}"#), None);
        let listed = r#"{"teams":{"T1":{"name":"Acme","url":"https://acme.enterprise.slack.com/",
            "enterprise_id":"E9"}},"lastActiveTeamId":"T1"}"#;
        assert_eq!(slack_session(listed), None);
        assert_eq!(slack_session(r#"{"teams":{"T1":{"url":"https://a.slack.com/","token":""}}}"#), None);
    }

    #[test]
    fn local_config_that_is_not_what_slack_writes_is_no_session() {
        for config in ["", "null", "{", "not json", "[]", "{}", r#"{"teams":[]}"#, r#"{"teams":"x"}"#,
            r#"{"teams":{"T1":"x","T2":7}}"#]
        {
            assert_eq!(slack_session(config), None, "{config:?}");
        }
    }

    #[test]
    fn discord_login_is_left_alone_until_the_page_moves_on() {
        assert!(hands_off("https://discord.com/login"));
        assert!(hands_off("https://discord.com/login?redirect_to=%2Fchannels%2F%40me"));
        assert!(hands_off("https://discord.com/login/"));
        assert!(!hands_off("https://discord.com/channels/@me"));
        assert!(!hands_off("https://discord.com/app"));
        assert!(!hands_off("https://slack.com/signin"));
        assert!(DISCORD.probe.contains("iframe"));
        assert!(DISCORD.probe.contains("localStorage.getItem('token')"));
    }

    #[test]
    fn a_discord_token_is_read_as_the_page_gave_it() {
        assert_eq!((DISCORD.read)(r#"{"token":"abc"}"#), Some(serde_json::json!({"token": "abc"})));
        assert_eq!((DISCORD.read)("nope"), None);
    }
}
