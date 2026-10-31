//! Linux handoff of invite links to an existing window.
//!
//! The socket lives in a private, user-owned directory. The process holding the
//! lock is the only one allowed to remove an old socket, and the sender checks
//! the peer's kernel-reported uid before it sends the bearer credential.

#![cfg(target_os = "linux")]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const MAX_LINK: usize = 2100;

pub enum Handoff {
    /// This process owns the endpoint and receives subsequent links.
    Primary(Receiver<String>),
    /// The existing window accepted the link.
    Delivered,
    /// The existing instance could not be reached. Keep the link for this window.
    Unavailable,
}

fn directory() -> io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let uid = unsafe { libc::geteuid() };
    // Isolate scratch profiles from the regular profile and from each other.
    let mut profile = std::collections::hash_map::DefaultHasher::new();
    protocol::support_dir().hash(&mut profile);
    let path = std::env::temp_dir().join(format!("tinyclient-invites-{uid}-{:016x}", profile.finish()));
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => (),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => (),
        Err(err) => return Err(err),
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe invite IPC directory"));
    }
    Ok(path)
}

fn same_user(stream: &UnixStream) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid writable storage for SO_PEERCRED.
    let result = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) };
    if result != 0 { return Err(io::Error::last_os_error()); }
    Ok(cred.uid == unsafe { libc::geteuid() })
}

fn send(path: &std::path::Path, link: &str) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    if !same_user(&stream)? { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invite receiver is another user")); }
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&(link.len() as u32).to_be_bytes())?;
    stream.write_all(link.as_bytes())?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    if ack != [1] { return Err(io::Error::other("invite was not accepted")); }
    Ok(())
}

fn receive(mut stream: UnixStream, sender: &mpsc::SyncSender<String>) -> io::Result<()> {
    if !same_user(&stream)? { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invite sender is another user")); }
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut len = [0; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_LINK { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid invite length")); }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes)?;
    let link = String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid invite encoding"))?;
    if !link.starts_with("tinyclient://join#") || protocol::parse_invite_link(&link).is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid invite"));
    }
    sender.try_send(link).map_err(|_| io::Error::other("invite queue unavailable"))?;
    stream.write_all(&[1])
}

/// Elect one receiver per user. No invite data is ever written to disk.
pub fn start(invite: Option<&str>) -> Handoff {
    let Ok(dir) = directory() else { return Handoff::Unavailable; };
    let lock = OpenOptions::new().write(true).create(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(dir.join("window.lock"));
    let Ok(lock) = lock else { return Handoff::Unavailable; };
    let socket = dir.join("window.sock");
    if lock.try_lock().is_err() {
        if let Some(link) = invite {
            // A primary may hold the lock before its listener is ready.
            for _ in 0..20 {
                if send(&socket, link).is_ok() { return Handoff::Delivered; }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        return Handoff::Unavailable;
    }
    let _ = fs::remove_file(&socket);
    let Ok(listener) = UnixListener::bind(&socket) else { return Handoff::Unavailable; };
    let (sender, receiver) = mpsc::sync_channel(8);
    std::thread::spawn(move || serve(listener, lock, sender));
    Handoff::Primary(receiver)
}

fn serve(listener: UnixListener, _lock: File, sender: mpsc::SyncSender<String>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => { let _ = receive(stream, &sender); }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_invite_does_not_enter_the_queue() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::sync_channel(8);
        let join = std::thread::spawn(move || receive(server, &tx));
        client.write_all(&3u32.to_be_bytes()).unwrap();
        client.write_all(b"bad").unwrap();
        assert!(join.join().unwrap().is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_valid_invite_is_acknowledged_and_delivered() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::sync_channel(8);
        let join = std::thread::spawn(move || receive(server, &tx));
        let url = protocol::invite_url("https://example.org", "token").unwrap();
        let link = url.replacen("https://tinyclient.io/invite#", "tinyclient://join#", 1);
        client.write_all(&(link.len() as u32).to_be_bytes()).unwrap();
        client.write_all(link.as_bytes()).unwrap();
        let mut ack = [0];
        client.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [1]);
        assert_eq!(rx.recv().unwrap(), link);
        join.join().unwrap().unwrap();
    }

    #[test]
    fn second_launch_delivers_to_the_primary() {
        let Handoff::Primary(receiver) = start(None) else { panic!("no primary receiver"); };
        let url = protocol::invite_url("https://example.org", "token").unwrap();
        let link = url.replacen("https://tinyclient.io/invite#", "tinyclient://join#", 1);
        assert!(matches!(start(Some(&link)), Handoff::Delivered));
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), link);
    }
}
