// The window opens with no console beside it on Windows. `--version` and `--text` borrow the
// terminal they were run from instead (`borrow_console`).
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod client;
#[cfg(any(feature = "gui", test))]
mod frames;
#[cfg(feature = "gui")]
mod gui;
mod view;

use client::ask;
use protocol::{chat_sock_path, InboxFilter, Request, Response, PAGE_LIMIT};
use std::io::{self, ErrorKind};
use std::process::ExitCode;

fn unexpected(response: Response) -> io::Error {
    io::Error::other(format!("unexpected {response:?}"))
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
    #[cfg(all(windows, feature = "gui"))]
    if std::env::args().any(|arg| arg == "--version" || arg == "--text") {
        borrow_console();
    }
    // Before the window opens: it is how an installed package proves it runs, with no display.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("tinyclient {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    #[cfg(feature = "gui")]
    if !std::env::args().any(|arg| arg == "--text") {
        gui::run();
        return ExitCode::SUCCESS;
    }

    match print_status() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            eprintln!("chatd is not running at {}", chat_sock_path().display());
            eprintln!("start it with: cargo run -p chatd");
            ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}
