//! One download: which hosts a network's files may come from, who may be handed the session's
//! headers, and the bytes going from the socket to a file through one fixed buffer.

use crate::adapters::{Command, Download, Refusal};
use protocol::{MediaFile, MediaKind, Network};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;
use ureq::tls::{TlsConfig, TlsProvider};

/// How long an adapter has to say where a file is.
const ANSWER: Duration = Duration::from_secs(30);
/// Bytes held of a file at any moment.
pub const BUFFER: usize = 64 * 1024;

const MB: u64 = 1024 * 1024;

/// What a host may be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The address alone. Signed addresses carry their own proof.
    Open,
    /// The address and the headers the adapter adds from the session.
    Session,
}

/// The hosts files are fetched from. Nothing else is ever contacted, whatever an adapter or a
/// message says.
pub struct Hosts {
    /// Hosts a test or the demo serves files from, over plain HTTP.
    pub local: Vec<(String, Trust)>,
}

/// A host that is `name` or ends in `.name`.
fn under(host: &str, name: &str) -> bool {
    host == name || host.strip_suffix(name).is_some_and(|rest| rest.ends_with('.'))
}

impl Hosts {
    fn trust(&self, network: Network, url: &str) -> Option<Trust> {
        let (secure, host) = host_of(url)?;
        if let Some((_, trust)) = self.local.iter().find(|(local, _)| *local == host) {
            return Some(*trust);
        }
        if !secure {
            return None;
        }
        let (open, session): (&[&str], &[&str]) = match network {
            // The CDN and the media proxy. Their addresses are signed and expire.
            Network::Discord => (&["cdn.discordapp.com", "discordapp.net"], &[]),
            // Avatars, emoji and unfurled images are public; files need the workspace's token.
            Network::Slack => (&["slack-edge.com", "slack-imgs.com", "slack-files.com"], &["files.slack.com"]),
            // Pictures in messages and people's pictures, each behind the session's own token.
            Network::Teams => (&[], &["asm.skype.com", "asyncgw.teams.microsoft.com", "teams.microsoft.com"]),
            Network::Telegram => (&[], &[]),
        };
        if session.iter().any(|name| under(&host, name)) {
            Some(Trust::Session)
        } else if open.iter().any(|name| under(&host, name)) {
            Some(Trust::Open)
        } else {
            None
        }
    }
}

/// Whether an address is `https`, and its host in lower case. `None` for anything that is not
/// a plain host name: a user part, a bracketed address, an odd character.
fn host_of(url: &str) -> Option<(bool, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let secure = match scheme {
        "https" => true,
        "http" => false,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|digit| digit.is_ascii_digit()) => host,
        Some(_) => return None,
        None => authority,
    };
    let plain = !host.is_empty() && host.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-');
    plain.then(|| (secure, host.to_ascii_lowercase()))
}

/// The most bytes a file of this kind may be. A body that says it is larger, or turns out to
/// be, is refused.
pub fn cap(file: &MediaFile, cache: u64) -> u64 {
    // What the network said the size is, with room for it being a little off.
    let declared = match file.bytes {
        0 => 0,
        bytes => bytes + bytes / 8 + 64 * 1024,
    };
    let cap = match file.kind {
        MediaKind::Avatar | MediaKind::Icon | MediaKind::Emoji => MB,
        // A thumbnail, or an original the network gave a size for.
        MediaKind::Image => declared.max(8 * MB),
        MediaKind::Video | MediaKind::Gif | MediaKind::Audio | MediaKind::File if declared > 0 => declared,
        MediaKind::Video | MediaKind::Gif | MediaKind::Audio | MediaKind::File => 100 * MB,
    };
    cap.min(cache)
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // A large file on a slow line is still one download, so no limit on the whole call.
        .timeout_recv_body(Some(Duration::from_secs(600)))
        // A redirect could lead anywhere, so none is followed here: `get` follows one itself,
        // and only to a host it would have asked in the first place.
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_connections(IDLE)
        // The same browser the session's other requests name; a second identity stands out.
        .user_agent(crate::adapters::net::browser())
        .build()
        .into()
}

/// Connections kept open between downloads: one per worker.
const IDLE: usize = super::IN_FLIGHT;

enum Failed {
    /// 403 or 404: the address has expired, or was never good.
    Gone(u16),
    Other(String),
}

/// Asks the account's adapter where the file is now and what the session adds.
fn ask(adapter: Option<&Sender<Command>>, id: &str, url: &str) -> Result<Download, String> {
    let adapter = adapter.ok_or("The account is not connected.")?;
    let (reply, answer) = channel();
    let command = Command::Download {
        id: id.to_owned(),
        url: url.to_owned(),
        reply,
    };
    adapter.send(command).map_err(|_| "The account is not connected.")?;
    match answer.recv_timeout(ANSWER) {
        Ok(Ok(download)) => Ok(download),
        Ok(Err(Refusal::Refused(why))) => Err(why),
        Ok(Err(Refusal::Unsupported)) => Err("This network's files cannot be fetched yet.".to_owned()),
        Err(_) => Err("The network did not answer.".to_owned()),
    }
}

/// What one download needs to know.
pub struct Want<'a> {
    pub network: Network,
    pub file: &'a MediaFile,
    /// The address the adapter registered.
    pub url: &'a str,
    pub adapter: Option<&'a Sender<Command>>,
    /// Where the bytes go while they arrive. The caller renames it once it is whole.
    pub part: &'a Path,
    pub cap: u64,
    /// Other files of the account waiting to be fetched, as `(id, address)`, read only when
    /// an address has to be renewed.
    pub also: &'a dyn Fn() -> Vec<(String, String)>,
}

/// True for an address that says of itself that it has run out, so asking it is a wasted
/// request: Discord signs attachment addresses with `ex`, the time they end, in hex.
fn run_out(network: Network, url: &str) -> bool {
    if network != Network::Discord {
        return false;
    }
    let ends = url.split(['?', '&']).find_map(|part| i64::from_str_radix(part.strip_prefix("ex=")?, 16).ok());
    ends.is_some_and(|ends| ends < crate::hub::now() + 60)
}

/// Asks the adapter for a fresh address for a file whose own has run out. Every other waiting
/// file whose address has run out too is asked about first, so the adapter can renew them in
/// one request to the network and record them (`Context::media_refreshed`); their answers are
/// not waited for.
fn renew(want: &Want) -> Result<Download, String> {
    if let Some(adapter) = want.adapter {
        let others = (want.also)();
        for (id, url) in others.into_iter().filter(|(id, url)| *id != want.file.id && run_out(want.network, url)) {
            let (reply, _) = channel();
            let _ = adapter.send(Command::Download { id, url, reply });
        }
    }
    ask(want.adapter, &want.file.id, want.url)
}

fn expects_image(kind: MediaKind) -> bool {
    matches!(kind, MediaKind::Avatar | MediaKind::Icon | MediaKind::Emoji | MediaKind::Image)
}

/// Redirects followed for one file. Each one must lead to a host of the same network.
const REDIRECTS: usize = 3;

/// Where a redirect leads: an address of its own, or a path on the host that sent it.
fn redirected(from: &str, location: &str) -> Option<String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_owned());
    }
    let (scheme, rest) = from.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    location.starts_with('/').then(|| format!("{scheme}://{authority}{location}"))
}

/// One GET, streamed into `want.part`. Headers go only to a host this network's sessions are for.
///
/// A redirect is followed only to another host this network serves files from, checked the same
/// way as the first: Slack answers some avatar addresses with the address of the picture.
/// Anything else a redirect names is refused unasked.
fn get(hosts: &Hosts, agent: &ureq::Agent, want: &Want, from: &Download, buffer: &mut [u8]) -> Result<u64, Failed> {
    let elsewhere = || Failed::Other(format!("{} does not serve files from that address.", want.network.label()));
    let mut url = from.url.clone();
    let mut hops = 0;
    let mut response = loop {
        let Some(trust) = hosts.trust(want.network, &url) else {
            // The host alone, for the log: it is how a wrong guess about a network gets found.
            let host = url.split("://").nth(1).and_then(|rest| rest.split(['/', '?', '#']).next()).unwrap_or("?");
            eprintln!("chatd: {}: a file was at or moved to {host}, which is not on the list", want.network.label());
            return Err(elsewhere());
        };
        let mut request = agent.get(&url);
        if trust == Trust::Session {
            for (name, value) in &from.headers {
                request = request.header(name.as_str(), value.as_str());
            }
        }
        let response = request.call().map_err(|err| Failed::Other(format!("The download failed: {err}.")))?;
        match response.status().as_u16() {
            200 => break response,
            gone @ (403 | 404) => return Err(Failed::Gone(gone)),
            moved @ (301 | 302 | 303 | 307 | 308) if hops < REDIRECTS => {
                let location = response.headers().get("location").and_then(|value| value.to_str().ok());
                url = location.and_then(|location| redirected(&url, location)).ok_or_else(|| Failed::Other(format!("The server answered {moved}.")))?;
                hops += 1;
            }
            other => return Err(Failed::Other(format!("The server answered {other}."))),
        }
    };
    let body = response.body();
    let mime = body.mime_type().unwrap_or("").to_ascii_lowercase();
    // A sign-in page where a file should be is the usual way a session shows it has ended.
    if mime == "text/html" || (expects_image(want.file.kind) && !mime.starts_with("image/")) {
        return Err(Failed::Other(format!("The server sent {mime:?}, not the file.")));
    }
    let too_large = || Failed::Other(format!("The file is larger than {} MB.", want.cap.div_ceil(MB)));
    if body.content_length().is_some_and(|length| length > want.cap) {
        return Err(too_large());
    }
    let broken = |err: std::io::Error| Failed::Other(format!("The download failed: {err}."));
    let mut out = File::create(want.part).map_err(broken)?;
    let mut reader = response.body_mut().as_reader();
    let mut total = 0u64;
    loop {
        let read = reader.read(buffer).map_err(broken)?;
        if read == 0 {
            return Ok(total);
        }
        total += read as u64;
        if total > want.cap {
            return Err(too_large());
        }
        out.write_all(&buffer[..read]).map_err(broken)?;
    }
}

/// Fetches one file into `want.part` and answers its size and the address it came from, or why
/// it could not be fetched, in words the user can read. Whatever a failure left of the part
/// file is removed.
pub fn download(hosts: &Hosts, agent: &ureq::Agent, want: &Want, buffer: &mut [u8]) -> Result<(u64, String), String> {
    let fetched = fetch(hosts, agent, want, buffer);
    if fetched.is_err() {
        let _ = std::fs::remove_file(want.part);
    }
    fetched
}

fn fetch(hosts: &Hosts, agent: &ureq::Agent, want: &Want, buffer: &mut [u8]) -> Result<(u64, String), String> {
    let gone = |status: u16| format!("The file is no longer there ({status}).");
    let explain = |failed: Failed| match failed {
        Failed::Gone(status) => gone(status),
        Failed::Other(why) => why,
    };
    let registered = Download {
        url: want.url.to_owned(),
        headers: Vec::new(),
    };
    match hosts.trust(want.network, want.url) {
        None => Err(format!("{} does not serve files from that address.", want.network.label())),
        // Only the adapter holds the session, so it says how.
        Some(Trust::Session) => {
            let from = ask(want.adapter, &want.file.id, want.url)?;
            let bytes = get(hosts, agent, want, &from, buffer).map_err(explain)?;
            Ok((bytes, from.url))
        }
        Some(Trust::Open) => {
            // An address that says it has run out is not asked: that would be a refusal from
            // the CDN for nothing, every time.
            let first = match run_out(want.network, want.url) {
                true => Err(Failed::Gone(404)),
                false => get(hosts, agent, want, &registered, buffer),
            };
            match first {
                Ok(bytes) => Ok((bytes, registered.url)),
                // A signed address that ran out: the adapter can ask the network for a fresh one.
                Err(Failed::Gone(status)) => match renew(want) {
                    Ok(fresh) if fresh.url != want.url => {
                        let bytes = get(hosts, agent, want, &fresh, buffer).map_err(explain)?;
                        Ok((bytes, fresh.url))
                    }
                    _ => Err(gone(status)),
                },
                Err(other) => Err(explain(other)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust(network: Network, url: &str) -> Option<Trust> {
        Hosts { local: vec![("127.0.0.1".to_owned(), Trust::Open)] }.trust(network, url)
    }

    /// A file host on localhost. Answers 404 under `/old` and four bytes anywhere else, and
    /// sends back each request's path and whether it carried the session.
    fn file_host() -> (u16, std::sync::mpsc::Receiver<(String, bool)>) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen, asked) = channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let (mut path, mut session, mut line) = (String::new(), false, String::new());
                while reader.read_line(&mut line).unwrap_or(0) > 2 {
                    if path.is_empty() {
                        path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                    }
                    session |= line.to_ascii_lowercase().starts_with("authorization: bearer xoxc-1");
                    line.clear();
                }
                let gone = path.starts_with("/old");
                let _ = seen.send((path, session));
                let answer: &[u8] = match gone {
                    true => b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                    false => b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: image/png\r\nContent-Length: 4\r\n\r\nfile",
                };
                let _ = reader.into_inner().write_all(answer);
            }
        });
        (port, asked)
    }

    /// An adapter that answers every `Download` with `to` and the session's header, and counts
    /// how often it was asked.
    fn adapter(to: String) -> (Sender<Command>, std::sync::mpsc::Receiver<String>) {
        let (commands, inbox) = channel();
        let (seen, asked) = channel();
        std::thread::spawn(move || {
            for command in inbox {
                if let Command::Download { url, reply, .. } = command {
                    let _ = seen.send(url);
                    let _ = reply.send(Ok(Download {
                        url: to.clone(),
                        headers: vec![("Authorization".to_owned(), "Bearer xoxc-1".to_owned())],
                    }));
                }
            }
        });
        (commands, asked)
    }

    fn fetched(hosts: &Hosts, network: Network, url: &str, adapter: Option<&Sender<Command>>) -> Result<Vec<u8>, String> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let part = std::env::temp_dir().join(format!("tinychat-fetch-{}-{next}.part", std::process::id()));
        let file = MediaFile::new("F1", MediaKind::Image);
        let want = Want {
            network,
            file: &file,
            url,
            adapter,
            part: &part,
            cap: MB,
            also: &Vec::new,
        };
        let done = download(hosts, &agent(), &want, &mut vec![0u8; BUFFER]).map(|_| std::fs::read(&part).unwrap());
        assert_eq!(done.is_err(), !part.exists(), "a failure leaves no part file");
        let _ = std::fs::remove_file(&part);
        done
    }

    #[test]
    fn a_file_that_needs_the_session_goes_through_the_adapter_and_only_its_host_gets_the_headers() {
        let (port, asked) = file_host();
        // `localhost` stands for the host a network keeps files on, `127.0.0.1` for its CDN.
        let hosts = Hosts {
            local: vec![("localhost".to_owned(), Trust::Session), ("127.0.0.1".to_owned(), Trust::Open)],
        };
        let private = format!("http://localhost:{port}/files-pri/a.png");
        let (commands, downloads) = adapter(private.clone());
        assert_eq!(fetched(&hosts, Network::Slack, &private, Some(&commands)).unwrap(), b"file");
        assert_eq!(downloads.try_recv().unwrap(), private);
        assert_eq!(asked.try_recv().unwrap(), ("/files-pri/a.png".to_owned(), true));

        // Not connected: nobody holds the session, so nothing is asked of the host at all.
        assert!(fetched(&hosts, Network::Slack, &private, None).is_err());
        assert!(asked.try_recv().is_err());

        // An adapter that points at a host outside the list is not followed, with or without
        // the session; one that points at an open host is, but the session stays behind.
        let (elsewhere, _) = adapter("https://example.org/a.png".to_owned());
        assert!(fetched(&hosts, Network::Slack, &private, Some(&elsewhere)).is_err());
        assert!(asked.try_recv().is_err());
        let (to_cdn, _) = adapter(format!("http://127.0.0.1:{port}/cdn/a.png"));
        assert_eq!(fetched(&hosts, Network::Slack, &private, Some(&to_cdn)).unwrap(), b"file");
        assert_eq!(asked.try_recv().unwrap(), ("/cdn/a.png".to_owned(), false));
    }

    #[test]
    fn an_expired_address_is_renewed_through_the_adapter_once() {
        let (port, asked) = file_host();
        let hosts = Hosts {
            local: vec![("127.0.0.1".to_owned(), Trust::Open)],
        };
        // Refused by the host with nothing in the address to say so: asked, then renewed.
        let old = format!("http://127.0.0.1:{port}/old/a.png");
        let (commands, downloads) = adapter(format!("http://127.0.0.1:{port}/new/a.png?ex=7fffffff"));
        assert_eq!(fetched(&hosts, Network::Discord, &old, Some(&commands)).unwrap(), b"file");
        assert_eq!(downloads.try_recv().unwrap(), old);
        let paths: Vec<String> = asked.try_iter().map(|(path, _)| path).collect();
        assert_eq!(paths, ["/old/a.png", "/new/a.png?ex=7fffffff"]);

        // An address that says itself it ran out (`ex`, the hex time) is not asked at all.
        let signed = format!("http://127.0.0.1:{port}/old/b.png?ex=1&is=1&hm=2");
        assert_eq!(fetched(&hosts, Network::Discord, &signed, Some(&commands)).unwrap(), b"file");
        let paths: Vec<String> = asked.try_iter().map(|(path, _)| path).collect();
        assert_eq!(paths, ["/new/a.png?ex=7fffffff"]);
        // That is Discord's way of signing: another network's address is simply asked.
        assert!(!run_out(Network::Slack, &signed) && run_out(Network::Discord, &signed));
        assert!(!run_out(Network::Discord, "https://cdn.discordapp.com/a.png?ex=7fffffff&is=1"));

        // The adapter has nothing newer, or there is no adapter: it failed, and is not asked twice.
        let (same, _) = adapter(old.clone());
        assert!(fetched(&hosts, Network::Discord, &old, Some(&same)).is_err());
        assert!(fetched(&hosts, Network::Discord, &old, None).is_err());
        assert_eq!(asked.try_iter().count(), 2);
    }

    #[test]
    fn the_other_waiting_files_that_ran_out_are_named_to_the_adapter_in_the_same_breath() {
        let (port, _) = file_host();
        let hosts = Hosts {
            local: vec![("127.0.0.1".to_owned(), Trust::Open)],
        };
        let address = |name: &str, ends: &str| format!("http://127.0.0.1:{port}/old/{name}.png?ex={ends}&is=1");
        let (commands, downloads) = adapter(format!("http://127.0.0.1:{port}/new/a.png?ex=7fffffff"));
        let part = std::env::temp_dir().join(format!("tinychat-fetch-{}-also.part", std::process::id()));
        let file = MediaFile::new("F1", MediaKind::Image);
        let url = address("a", "1");
        let waiting = vec![("F2".to_owned(), address("b", "1")), ("F3".to_owned(), address("c", "7fffffff")), ("F1".to_owned(), url.clone())];
        let want = Want {
            network: Network::Discord,
            file: &file,
            url: &url,
            adapter: Some(&commands),
            part: &part,
            cap: MB,
            also: &|| waiting.clone(),
        };
        download(&hosts, &agent(), &want, &mut vec![0u8; BUFFER]).unwrap();
        let _ = std::fs::remove_file(&part);
        // The one that ran out too goes first; the one still good and the file itself are not
        // asked about twice.
        let named: Vec<String> = downloads.try_iter().collect();
        assert_eq!(named, [address("b", "1"), url]);
    }

    #[test]
    fn only_a_networks_own_hosts_are_trusted_and_only_its_file_host_gets_the_session() {
        use Network::{Discord, Slack, Teams};
        assert_eq!(trust(Discord, "https://cdn.discordapp.com/avatars/1/a.webp?size=80"), Some(Trust::Open));
        assert_eq!(trust(Discord, "https://media.discordapp.net/attachments/1/2/a.png?ex=1&is=2&hm=3"), Some(Trust::Open));
        assert_eq!(trust(Slack, "https://files.slack.com/files-tmb/T1-F1/a_480.png"), Some(Trust::Session));
        assert_eq!(trust(Slack, "https://avatars.slack-edge.com/2024/a_72.png"), Some(Trust::Open));
        // One network's hosts are not another's.
        assert_eq!(trust(Discord, "https://files.slack.com/files-pri/T1-F1/a.png"), None);
        assert_eq!(trust(Slack, "https://cdn.discordapp.com/avatars/1/a.webp"), None);
        assert_eq!(trust(Teams, "https://cdn.discordapp.com/avatars/1/a.webp"), None);
        // Not HTTPS, a look-alike, a user part, a link from a message.
        assert_eq!(trust(Discord, "http://cdn.discordapp.com/avatars/1/a.webp"), None);
        assert_eq!(trust(Discord, "https://cdn.discordapp.com.example.org/a.png"), None);
        assert_eq!(trust(Discord, "https://evilcdn.discordapp.com/a.png"), None);
        assert_eq!(trust(Slack, "https://notslack-edge.com/a.png"), None);
        assert_eq!(trust(Slack, "https://files.slack.com@example.org/a.png"), None);
        assert_eq!(trust(Slack, "https://example.org\\@files.slack.com/a.png"), None);
        assert_eq!(trust(Discord, "https://example.org/cdn.discordapp.com/a.png"), None);
        assert_eq!(trust(Discord, "file:///etc/passwd"), None);
        // A test's own server, and nothing else on plain HTTP.
        assert_eq!(trust(Discord, "http://127.0.0.1:4000/a.png"), Some(Trust::Open));
        assert_eq!(trust(Discord, "http://localhost:4000/a.png"), None);
    }

    #[test]
    fn a_redirect_is_followed_only_to_a_host_the_network_serves_files_from() {
        use std::io::{BufRead, BufReader};
        // A host that sends every request on to wherever its path says.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let redirector = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let (mut path, mut line) = (String::new(), String::new());
                while reader.read_line(&mut line).unwrap_or(0) > 2 {
                    if path.is_empty() {
                        path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                    }
                    line.clear();
                }
                let to = path.trim_start_matches("/to/");
                let answer = format!("HTTP/1.1 302 Found\r\nConnection: close\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n");
                let _ = reader.into_inner().write_all(answer.as_bytes());
            }
        });
        let (port, asked) = file_host();
        let hosts = Hosts {
            local: vec![("127.0.0.1".to_owned(), Trust::Open)],
        };
        let via = |to: &str| format!("http://127.0.0.1:{redirector}/to/{to}");

        // To the picture, on a host that is trusted: fetched.
        let picture = format!("http://127.0.0.1:{port}/avatars/a_72.png");
        assert_eq!(fetched(&hosts, Network::Slack, &via(&picture), None).unwrap(), b"file");
        assert_eq!(asked.try_recv().unwrap().0, "/avatars/a_72.png");
        // To anywhere else: refused, and that host never hears of it.
        assert!(fetched(&hosts, Network::Slack, &via(&format!("http://localhost:{port}/a.png")), None).is_err());
        assert!(fetched(&hosts, Network::Slack, &via("https://example.org/a.png"), None).is_err());
        assert!(asked.try_recv().is_err());
        // Round and round: given up on.
        let itself = via(&via(&via(&via(&picture))));
        assert!(fetched(&hosts, Network::Slack, &itself, None).is_err());
        assert_eq!(redirected("https://ca.slack-edge.com/T1-U1-72", "/avatars/a.png").as_deref(), Some("https://ca.slack-edge.com/avatars/a.png"));
        assert_eq!(redirected("https://ca.slack-edge.com/T1-U1-72", "a.png"), None);
    }

    #[test]
    fn caps_follow_the_kind_and_what_the_network_said() {
        let file = |kind, bytes| MediaFile { bytes, ..MediaFile::new("f", kind) };
        let cache = 500 * MB;
        assert_eq!(cap(&file(MediaKind::Avatar, 0), cache), MB);
        assert_eq!(cap(&file(MediaKind::Avatar, 30 * MB), cache), MB);
        assert_eq!(cap(&file(MediaKind::Image, 0), cache), 8 * MB);
        assert!(cap(&file(MediaKind::Image, 20 * MB), cache) >= 20 * MB);
        assert!((20 * MB..24 * MB).contains(&cap(&file(MediaKind::File, 20 * MB), cache)));
        assert_eq!(cap(&file(MediaKind::Video, 0), cache), 100 * MB);
        assert_eq!(cap(&file(MediaKind::File, 800 * MB), cache), cache);
    }
}
