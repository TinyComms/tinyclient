//! Linux and Windows sign-in window (tao + wry).

use crate::teams;
use crate::{
    blocked, finish, load_failed_message, route, session, web_client, Route, Site, BLOCKED, CLOSED,
    LOAD_FAILED, NO_TOKEN, NO_TOKEN_AFTER, PAGE, SIZE,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebContext, WebView, WebViewBuilder};

struct State {
    refused: bool,
    on_client: Option<Instant>,
    current_url: String,
    pending_load: Option<String>,
    /// Slack session body waiting for its `d` cookie.
    pending_session: Option<serde_json::Value>,
}

fn data_dir(network: &str) -> PathBuf {
    let root = platform_data_root();
    let dir = root.join("tinyclient").join("login").join(network);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn platform_data_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local").join("share");
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn run(site: Site, at: Option<(f64, f64)>, network: &str) {
    let agent = crate::user_agent();
    let mut web_context = WebContext::new(Some(data_dir(network)));

    let event_loop = EventLoop::new();
    let mut builder = WindowBuilder::new()
        .with_title(site.title)
        .with_inner_size(LogicalSize::new(SIZE.0, SIZE.1))
        .with_resizable(true);
    if let Some((x, y)) = at {
        builder = builder.with_position(LogicalPosition::new(x - SIZE.0 / 2., y - SIZE.1 / 2.));
    }
    let window = builder.build(&event_loop).expect("window");

    let state = Arc::new(Mutex::new(State {
        refused: false,
        on_client: None,
        current_url: String::new(),
        pending_load: None,
        pending_session: None,
    }));

    let nav_state = state.clone();
    let built = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url(site.start)
        .with_user_agent(&agent)
        .with_navigation_handler({
            let nav_state = nav_state.clone();
            move |url| {
                teams::navigated(&url);
                let from = nav_state.lock().unwrap().current_url.clone();
                let from = if from.is_empty() { None } else { Some(from.as_str()) };
                match route(&url, from) {
                    Route::Allow => true,
                    Route::Rewrite(instead) => {
                        nav_state.lock().unwrap().pending_load = Some(instead);
                        false
                    }
                    Route::Block => false,
                }
            }
        })
        .with_new_window_req_handler({
            let nav_state = nav_state.clone();
            move |url, _| {
                teams::navigated(&url);
                let from = nav_state.lock().unwrap().current_url.clone();
                let from = if from.is_empty() { None } else { Some(from.as_str()) };
                match route(&url, from) {
                    Route::Allow => nav_state.lock().unwrap().pending_load = Some(url),
                    Route::Rewrite(instead) => nav_state.lock().unwrap().pending_load = Some(instead),
                    Route::Block => {}
                }
                wry::NewWindowResponse::Deny
            }
        })
        .with_on_page_load_handler({
            let state = state.clone();
            move |event, url| {
                if matches!(event, PageLoadEvent::Started) {
                    state.lock().unwrap().current_url = url.clone();
                }
                if matches!(event, PageLoadEvent::Finished) && load_failed_message(&url) {
                    eprintln!("tinyclient-login: the page did not load: {url}");
                    std::process::exit(LOAD_FAILED);
                }
            }
        })
        .build(&window)
        .expect("webview");

    let webview = Arc::new(Mutex::new(built));
    let site = site;
    let agent_loop = agent.clone();
    let mut last_tick = Instant::now();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if last_tick.elapsed() >= Duration::from_secs(1) {
            last_tick = Instant::now();
            let web = webview.lock().unwrap();
            tick(&web, site, &agent_loop, &state);
        }

        if let Some(url) = state.lock().unwrap().pending_load.take() {
            let web = webview.lock().unwrap();
            let _ = web.load_url(&url);
        }

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            let refused = state.lock().unwrap().refused;
            std::process::exit(if refused { BLOCKED } else { CLOSED });
        }
    });
}

fn tick(web: &WebView, site: Site, agent: &str, state: &Arc<Mutex<State>>) {
    if let Some(wanted) = site.cookie {
        let mut guard = state.lock().unwrap();
        if let Some(probed) = guard.pending_session.take() {
            if let Ok(cookies) = web.cookies() {
                let cookie = cookies
                    .iter()
                    .find(|cookie| cookie.name() == wanted)
                    .map(|cookie| cookie.value().to_string());
                if cookie.is_some() {
                    finish(session(probed, cookie, agent));
                } else {
                    guard.pending_session = Some(probed);
                }
            } else {
                guard.pending_session = Some(probed);
            }
        }
    }

    if let Ok(at) = web.url() {
        if let Some((path, instead)) = site.detour {
            if at.contains(path) {
                let _ = web.load_url(instead);
                return;
            }
        }
        let mut guard = state.lock().unwrap();
        if web_client(&at) {
            let since = guard.on_client.get_or_insert_with(Instant::now);
            if since.elapsed() >= NO_TOKEN_AFTER {
                eprintln!(
                    "tinyclient-login: signed in to Slack, but the browser holds no token to read after \
                     {} seconds; Enterprise Grid organizations are known to do this",
                    NO_TOKEN_AFTER.as_secs()
                );
                std::process::exit(NO_TOKEN);
            }
        }
    }

    let state_page = state.clone();
    let _ = web.evaluate_script_with_callback(PAGE, move |value| {
        let Some(text) = js_return(&value) else {
            return;
        };
        let Ok(page) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        let url = page["url"].as_str().unwrap_or("");
        let text = page["text"].as_str().unwrap_or("");
        let mut guard = state_page.lock().unwrap();
        if blocked(url, text) && !guard.refused {
            guard.refused = true;
            eprintln!("tinyclient-login: the organization does not allow this sign-in");
        }
    });

    let probe = site.probe;
    let read = site.read;
    let wanted = site.cookie;
    let agent = agent.to_owned();
    let state_probe = state.clone();
    let _ = web.evaluate_script_with_callback(probe, move |value| {
        let Some(text) = js_return(&value) else {
            return;
        };
        let Some(probed) = (read)(&text) else {
            return;
        };
        let Some(_wanted) = wanted else {
            finish(session(probed, None, &agent));
        };
        state_probe.lock().unwrap().pending_session = Some(probed);
    });
}

fn js_return(value: &str) -> Option<String> {
    if value.is_empty() || value == "null" || value == "undefined" {
        return None;
    }
    serde_json::from_str(value)
        .ok()
        .or_else(|| Some(value.to_owned()))
}
