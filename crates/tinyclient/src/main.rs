#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod client;
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

fn main() -> ExitCode {
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
