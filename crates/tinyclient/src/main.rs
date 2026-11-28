// The window opens with no console beside it on Windows. `--version` and `--text` borrow the
// terminal they were run from instead (`borrow_console`).
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod client;
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod host;
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod update;
#[cfg(any(feature = "gui", test))]
mod frames;
#[cfg(any(feature = "gui", test))]
mod tiny_attach;
// The Logs window's model; without a window, only its tests use it.
#[cfg(any(feature = "gui", test))]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod log_tail;
// The window draws what it says; without one, only its tests use it.
#[cfg(any(feature = "gui", test))]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod tiny_setup;
#[cfg(feature = "gui")]
mod gui;
#[cfg(all(test, feature = "gui"))]
#[path = "mobile/share.rs"]
mod mobile_share;
#[cfg(feature = "gui")]
mod lottie;
#[cfg(all(feature = "gui", target_os = "linux"))]
mod invite_ipc;
#[cfg(all(feature = "gui", windows))]
#[path = "invite_ipc_windows.rs"]
mod invite_ipc;
// The quick actions (jump list, Dock menu, desktop Actions) and their launch arguments.
#[cfg(any(feature = "gui", test))]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod quick;
mod view;

use client::ask;
use protocol::{chat_sock_path, InboxFilter, Request, Response, PAGE_LIMIT};
use std::io::{self, ErrorKind};
use std::process::ExitCode;

fn unexpected(_response: Response) -> io::Error {
    io::Error::other("unexpected response")
}

/// Only a valid Tiny app link is accepted from the operating system.
fn launch_invite(args: &[String]) -> Option<String> {
    args.iter()
        .find(|arg| {
            (arg.starts_with("tinyclient://join#") && protocol::parse_invite_link(arg).is_some())
                || protocol::parse_room_link(arg).is_some()
                || protocol::parse_message_link(arg).is_some()
        })
        .cloned()
}

/// The shell without a window: the same states, as lines.
fn print_status() -> io::Result<()> {
    let (networks, media, call) = match ask(&Request::Status)? {
        Response::Status { networks, media, call, .. } => (networks, media, call),
        other => return Err(unexpected(other)),
    };
    for net in networks {
        println!("{:?} {:?}", net.network, net.link);
    }
    println!("media {media:?}");
    if let Some(call) = call {
        println!("call {} {}", call.conversation.network.label(), call.conversation.id);
    }
    let inbox = Request::Inbox {
        network: None,
        filter: InboxFilter::All,
        offset: 0,
        limit: PAGE_LIMIT,
    };
    let conversations = match ask(&inbox)? {
        Response::Conversations { conversations, .. } => conversations,
        other => return Err(unexpected(other)),
    };
    let now = view::now();
    for c in conversations {
        let unread = view::row_badge(&c).unwrap_or_default();
        println!(
            "{:<8} {:<24} {:>3} {:>4}  {}",
            c.key.network.label(),
            c.title,
            unread,
            view::age(now, c.updated),
            c.preview
        );
    }
    Ok(())
}

/// The shell without a window: what a search of the cache finds, as lines.
fn print_search(query: &str) -> io::Result<()> {
    let request = Request::SearchMessages { conversation: None, network: None, space: None, query: query.to_owned(), limit: PAGE_LIMIT };
    let hits = match ask(&request)? {
        Response::MessageHits { hits, .. } => hits,
        other => return Err(unexpected(other)),
    };
    let now = view::now();
    for hit in hits {
        println!(
            "{:<8} {:<24} {:>4}  {}",
            hit.conversation.network.label(),
            hit.title,
            view::age(now, hit.sent),
            if hit.age_restricted { "Age-restricted channel" } else { hit.snippet.as_str() }
        );
    }
    Ok(())
}

/// Parses `--export <target>` into an `ExportScope`: `"all"` exports everything cached on this
/// device, and `<network>:<account>:<conversation_id>` exports one conversation.
fn parse_export_scope(target: &str) -> Result<protocol::ExportScope, String> {
    let trimmed = target.trim();
    if trimmed.eq_ignore_ascii_case("all") {
        return Ok(protocol::ExportScope::All);
    }
    let mut parts = trimmed.splitn(3, ':');
    let net_str = parts.next().unwrap_or("");
    let account = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("");
    if account.is_empty() || id.is_empty() {
        return Err("expected `all` or `<network>:<account>:<conversation_id>`".to_owned());
    }
    let network = match net_str.to_ascii_lowercase().as_str() {
        "discord" => protocol::Network::Discord,
        "slack" => protocol::Network::Slack,
        "telegram" => protocol::Network::Telegram,
        "teams" => protocol::Network::Teams,
        "signal" => protocol::Network::Signal,
        "messages" => protocol::Network::Messages,
        "tiny" => protocol::Network::Tiny,
        _ => return Err(format!("unknown network `{net_str}`")),
    };
    Ok(protocol::ExportScope::Conversation {
        conversation: protocol::Key { network, account: account.to_owned(), id: id.to_owned() },
    })
}

/// `--text --export <conversation|all>`: writes an unencrypted local cache export to disk.
fn run_export(target: &str, out_dir: Option<&str>, include_files: bool) -> io::Result<()> {
    let scope = parse_export_scope(target).map_err(|why| io::Error::new(ErrorKind::InvalidInput, why))?;
    let destination = match out_dir {
        Some(dir) => dir.to_owned(),
        None => {
            let now = view::now();
            protocol::support_dir()
                .join("exports")
                .join(format!("export-{now}"))
                .to_string_lossy()
                .into_owned()
        }
    };
    let request = Request::Export {
        scope,
        destination,
        since: None,
        until: None,
        include_files,
    };
    match ask(&request)? {
        Response::Exported { path, conversations, messages, files, earliest, latest } => {
            println!("Exported {messages} cached messages and {files} files across {conversations} conversation(s) to {path}");
            if let (Some(a), Some(b)) = (earliest, latest) {
                println!("Cached range: {a}..={b} (unix seconds)");
            }
            println!("Warning: this export is unencrypted on disk (including messages that were end-to-end encrypted in transit).");
            Ok(())
        }
        Response::Error { detail, .. } => Err(io::Error::other(detail)),
        other => Err(unexpected(other)),
    }
}

/// `--notify-test`: asks chatd to post the test notification (N13) and says what came of it.
fn notify_test() -> io::Result<()> {
    match ask(&Request::NotifyTest)? {
        Response::NotifyTested { posted: true, .. } => println!("posted, and listed by the system"),
        Response::NotifyTested { posted: false, detail } => println!("not posted: {detail}"),
        other => return Err(unexpected(other)),
    }
    Ok(())
}

/// A windowed program starts with no console, so what it prints goes nowhere. When output is not
/// already a pipe or a file, write to the console of the program that started it, if any.
#[cfg(all(windows, feature = "gui"))]
fn borrow_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    let missing = |handle: HANDLE| handle.is_null() || handle == INVALID_HANDLE_VALUE;
    // SAFETY: plain Win32 calls on handles this process owns; std reads the standard handles
    // afresh at each write, so setting them before anything is printed is enough.
    unsafe {
        let wanted: Vec<_> = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].into_iter().filter(|&which| missing(GetStdHandle(which))).collect();
        if wanted.is_empty() || AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let console = CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
        if missing(console) {
            return;
        }
        for which in wanted {
            SetStdHandle(which, console);
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let query = match args.iter().position(|arg| arg == "--search") {
        Some(at) => match args.get(at + 1) {
            Some(query) => Some(query.as_str()),
            None => {
                tracing::warn!("tinyclient: --search needs what to look for");
                return ExitCode::from(64);
            }
        },
        None => None,
    };
    let export_target = match args.iter().position(|arg| arg == "--export") {
        Some(at) => match args.get(at + 1) {
            Some(target) => Some(target.as_str()),
            None => {
                tracing::warn!("tinyclient: --export needs a conversation (<network>:<account>:<id>) or `all`");
                return ExitCode::from(64);
            }
        },
        None => None,
    };
    let export_out = args.iter().position(|arg| arg == "--out").and_then(|at| args.get(at + 1)).map(String::as_str);
    let export_files = !args.iter().any(|arg| arg == "--no-files");
    #[cfg(all(windows, feature = "gui"))]
    if args.iter().any(|arg| arg == "--version" || arg == "--text" || arg == "--notify-test") || query.is_some() || export_target.is_some() {
        borrow_console();
    }
    // Before the window opens: it is how an installed package proves it runs, with no display.
    if args.first().map(String::as_str) == Some("--version") {
        println!("tinyclient {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    // The lab's save-downloads driver. A build without this opens the window below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("save-downloads") {
        if let Err(err) = host::prove_save_downloads() {
            eprintln!("save-downloads: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's error-text driver. A build without this opens the window below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("error-text") {
        if let Err(err) = client::prove_error_text() {
            eprintln!("error-text: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's no-console driver. A build without this opens the window below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("no-console") {
        let swap = update::windows_swap_args(std::path::Path::new("apply.ps1"));
        if update::helper_console_flags() & 0x0800_0000 == 0 || swap.iter().any(|arg| arg == "start") {
            eprintln!("no-console: an update check would open a terminal");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's focus-ring driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("focus-ring") {
        if let Err(err) = gui::prove_focus_ring() {
            eprintln!("focus-ring: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's text-drag driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("text-drag") {
        if let Err(err) = gui::prove_text_drag() {
            eprintln!("text-drag: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's scroll-release driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("scroll-release") {
        if let Err(err) = gui::prove_scroll_release() {
            eprintln!("scroll-release: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's pane-focus-pointer driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("pane-focus-pointer") {
        if let Err(err) = gui::prove_pane_focus_pointer() {
            eprintln!("pane-focus-pointer: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's selection-clamp driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("selection-clamp") {
        if let Err(err) = gui::prove_selection_clamp() {
            eprintln!("selection-clamp: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's push-to-talk-release driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("push-to-talk-release") {
        if let Err(err) = gui::prove_push_to_talk_release() {
            eprintln!("push-to-talk-release: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's Tiny STUN-watchdog driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("stun-watchdog") {
        if let Err(err) = gui::prove_stun_watchdog() {
            eprintln!("stun-watchdog: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's health-grade driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("health-grade") {
        if let Err(err) = gui::prove_health_grade() {
            eprintln!("health-grade: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's compact-call-health driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("compact-call-health") {
        if let Err(err) = gui::prove_compact_call_health() {
            eprintln!("compact-call-health: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's call-device-ux driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("call-device-ux") {
        if let Err(err) = gui::prove_call_device_ux() {
            eprintln!("call-device-ux: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's cancel-join driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cancel-join") {
        if let Err(err) = gui::prove_cancel_join() {
            eprintln!("cancel-join: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's stale-page driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("stale-page") {
        if let Err(err) = gui::prove_stale_page() {
            eprintln!("stale-page: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's return-to-bottom driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("return-to-bottom") {
        if let Err(err) = gui::prove_return_to_bottom() {
            eprintln!("return-to-bottom: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's offline-send-kept driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("offline-send-kept") {
        if let Err(err) = gui::prove_offline_send_kept() {
            eprintln!("offline-send-kept: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's telegram-mute-bell driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-mute-bell") {
        if let Err(err) = gui::prove_telegram_mute_bell() {
            eprintln!("telegram-mute-bell: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's spell-check driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("spell-check") {
        if let Err(err) = gui::prove_spell_check() {
            eprintln!("spell-check: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's schedule-send driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("schedule-send") {
        if let Err(err) = gui::prove_schedule_send() {
            eprintln!("schedule-send: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's discord-format driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-format") {
        if let Err(err) = gui::prove_discord_format() {
            eprintln!("discord-format: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's shortcut-no-key driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("shortcut-no-key") {
        if let Err(err) = gui::prove_shortcut_no_key() {
            eprintln!("shortcut-no-key: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's open-at-login driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("open-at-login") {
        if let Err(err) = gui::prove_open_at_login() {
            eprintln!("open-at-login: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's command-palette driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("command-palette") {
        if let Err(err) = gui::prove_command_palette() {
            eprintln!("command-palette: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's screen-lock driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("screen-lock") {
        if let Err(err) = gui::prove_screen_lock() {
            eprintln!("screen-lock: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's message-ux-polish driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("message-ux-polish") {
        if let Err(err) = gui::prove_message_ux_polish() {
            eprintln!("message-ux-polish: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's tiny-call-ux driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-call-ux") {
        if let Err(err) = gui::prove_tiny_call_ux() {
            eprintln!("tiny-call-ux: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's ui-wave2-polish driver. A build without this opens the window below and stays up.
    #[cfg(feature = "gui")]
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("ui-wave2-polish") {
        if let Err(err) = gui::prove_ui_wave2_polish() {
            eprintln!("ui-wave2-polish: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    let _log = protocol::log_policy::install("tinyclient", protocol::log_policy::LogPolicy::from_env());
    #[cfg(feature = "gui")]
    if !args.iter().any(|arg| arg == "--text" || arg == "--notify-test") && query.is_none() && export_target.is_none() {
        let invite = launch_invite(&args);
        let action = quick::launch_action(&args);
        // A second launch hands the first one its invite, or a quick action, and exits.
        #[cfg(any(target_os = "linux", windows))]
        let handoff = invite.clone().or_else(|| action.map(quick::QuickAction::link));
        #[cfg(any(target_os = "linux", windows))]
        let incoming = match invite_ipc::start(handoff.as_deref()) {
            invite_ipc::Handoff::Delivered => return ExitCode::SUCCESS,
            invite_ipc::Handoff::Primary(receiver) => {
                // Nothing was running: a quick Quit has no window to close, so the launcher's
                // item ends here instead of opening one only to close it.
                if action == Some(quick::QuickAction::Quit) && invite.is_none() {
                    return ExitCode::SUCCESS;
                }
                Some(receiver)
            }
            invite_ipc::Handoff::Unavailable => None,
        };
        #[cfg(not(any(target_os = "linux", windows)))]
        let incoming = None;
        gui::run(invite, action, incoming);
        return ExitCode::SUCCESS;
    }

    let done = match (export_target, query) {
        (Some(target), _) => run_export(target, export_out, export_files),
        (None, Some(query)) => print_search(query),
        (None, None) if args.iter().any(|arg| arg == "--notify-test") => notify_test(),
        (None, None) => print_status(),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            tracing::warn!("chatd is not running at {}", chat_sock_path().display());
            tracing::warn!("start it with: cargo run -p chatd");
            ExitCode::from(1)
        }
        Err(err) => {
            tracing::warn!("{err}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod invite_launch_tests {
    use super::*;

    #[test]
    fn accepts_only_valid_app_invites() {
        let link = protocol::invite_url("https://example.org", "token").unwrap();
        let app = link.replacen("https://tinyclient.io/invite#", "tinyclient://join#", 1);
        assert_eq!(launch_invite(&[app.clone()]), Some(app));
        let room = protocol::room_link("https://example.org", "lounge").unwrap();
        assert_eq!(launch_invite(&[room.clone()]), Some(room));
        assert_eq!(launch_invite(&[link, "tinyclient://join#broken".into(), "tinyclient://room?server=example.org&id=..".into()]), None);
    }

    #[test]
    fn parses_export_cli_targets() {
        assert_eq!(parse_export_scope("all"), Ok(protocol::ExportScope::All));
        assert_eq!(
            parse_export_scope("discord:acct1:chan9"),
            Ok(protocol::ExportScope::Conversation {
                conversation: protocol::Key { network: protocol::Network::Discord, account: "acct1".into(), id: "chan9".into() },
            })
        );
        assert!(parse_export_scope("bad").is_err());
        assert!(parse_export_scope("bogus:acct:chan").is_err());
    }
}
