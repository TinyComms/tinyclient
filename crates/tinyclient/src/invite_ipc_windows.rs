//! User-scoped named-pipe handoff for invitations on Windows.
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PeekNamedPipe, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};

const MAX_LINK: usize = 2100;

pub enum Handoff { Primary(Receiver<String>), Delivered, Unavailable }

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

fn pipe_name() -> Vec<u16> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    protocol::support_dir().hash(&mut hash);
    wide(&format!(r"\\.\pipe\tinyclient-invite-{:016x}", hash.finish()))
}

/// The first instance is elected atomically. Its pipe allows only its owner.
fn listen(name: &[u16], first: bool) -> io::Result<File> {
    let mut descriptor = std::ptr::null_mut();
    let sddl = wide("D:P(A;;GA;;;OW)");
    // SAFETY: all pointers are valid for the duration of these calls; Windows allocates descriptor.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) == 0 {
            return Err(io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
        let flags = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let handle = CreateNamedPipeW(name.as_ptr(), flags, PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 2, 4096, 4096, 0, &attrs);
        LocalFree(descriptor);
        if handle == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
        Ok(File::from_raw_handle(handle))
    }
}

/// Return token-user data in aligned storage; its SID points inside that storage.
unsafe fn user(process: HANDLE) -> io::Result<Vec<usize>> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 { return Err(io::Error::last_os_error()); }
    let mut len = 0;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 { unsafe { CloseHandle(token) }; return Err(io::Error::last_os_error()); }
    let mut data = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
    let ok = unsafe { GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), len, &mut len) };
    unsafe { CloseHandle(token) };
    if ok == 0 { return Err(io::Error::last_os_error()); }
    Ok(data)
}

/// Refuse a squatted pipe before writing the bearer token. Verify both its user
/// and executable, using the server PID reported by the kernel for this handle.
fn trusted_server(pipe: &File) -> bool {
    unsafe {
        let mut pid = 0;
        if GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) == 0 { return false; }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() { return false; }
        let same_user = match (user(GetCurrentProcess()), user(process)) {
            (Ok(a), Ok(b)) => {
                let a = &*(a.as_ptr() as *const TOKEN_USER);
                let b = &*(b.as_ptr() as *const TOKEN_USER);
                EqualSid(a.User.Sid, b.User.Sid) != 0
            }
            _ => false,
        };
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        let same_exe = if QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0 {
            let theirs = std::path::PathBuf::from(std::ffi::OsString::from_wide(&path[..length as usize]));
            std::env::current_exe().is_ok_and(|ours| ours.to_string_lossy().eq_ignore_ascii_case(&theirs.to_string_lossy()))
        } else { false };
        CloseHandle(process);
        same_user && same_exe
    }
}

use std::os::windows::ffi::OsStringExt;

fn read_exact_bounded(file: &mut File, out: &mut [u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut got = 0;
    while got < out.len() {
        let mut available = 0;
        let ok = unsafe { PeekNamedPipe(file.as_raw_handle(), std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut available, std::ptr::null_mut()) };
        if ok == 0 { return Err(io::Error::last_os_error()); }
        if available > 0 {
            let end = got + (available as usize).min(out.len() - got);
            let read = file.read(&mut out[got..end])?;
            if read == 0 { return Err(io::ErrorKind::UnexpectedEof.into()); }
            got += read;
        } else if Instant::now() >= deadline { return Err(io::ErrorKind::TimedOut.into()); }
        else { std::thread::sleep(Duration::from_millis(10)); }
    }
    Ok(())
}

fn send(name: &[u16], link: &str) -> io::Result<()> {
    let handle = unsafe { CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    if handle == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
    let mut file = unsafe { File::from_raw_handle(handle) };
    if !trusted_server(&file) { return Err(io::ErrorKind::PermissionDenied.into()); }
    file.write_all(&(link.len() as u32).to_be_bytes())?;
    file.write_all(link.as_bytes())?;
    let mut ack = [0];
    read_exact_bounded(&mut file, &mut ack)?;
    if ack != [1] { return Err(io::Error::other("invite not accepted")); }
    Ok(())
}

fn receive(file: &mut File, sender: &SyncSender<String>) -> io::Result<()> {
    let mut len = [0; 4];
    read_exact_bounded(file, &mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_LINK { return Err(io::ErrorKind::InvalidData.into()); }
    let mut data = vec![0; len];
    read_exact_bounded(file, &mut data)?;
    let link = String::from_utf8(data).map_err(|_| io::ErrorKind::InvalidData)?;
    if !link.starts_with("tinyclient://join#") || protocol::parse_invite_link(&link).is_none() { return Err(io::ErrorKind::InvalidData.into()); }
    sender.try_send(link).map_err(|_| io::Error::other("invite queue unavailable"))?;
    file.write_all(&[1])
}

pub fn start(invite: Option<&str>) -> Handoff {
    let name = pipe_name();
    match listen(&name, true) {
        Ok(pipe) => {
            let (sender, receiver) = mpsc::sync_channel(8);
            std::thread::spawn(move || serve(name, pipe, sender));
            Handoff::Primary(receiver)
        }
        Err(_) => {
            if let Some(link) = invite {
                for _ in 0..20 {
                    if send(&name, link).is_ok() { return Handoff::Delivered; }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            Handoff::Unavailable
        }
    }
}

fn serve(name: Vec<u16>, mut pipe: File, sender: SyncSender<String>) {
    loop {
        let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) };
        if ok != 0 || io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            let _ = receive(&mut pipe, &sender);
        }
        // Create the next instance before closing the previous one so a second
        // process cannot acquire the first-instance name between connections.
        match listen(&name, false) {
            Ok(next) => pipe = next,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_launch_delivers_to_the_primary() {
        let Handoff::Primary(receiver) = start(None) else { panic!("primary pipe unavailable"); };
        let url = protocol::invite_url("https://example.org", "token").unwrap();
        let link = url.replacen("https://tinyclient.io/invite#", "tinyclient://join#", 1);
        assert!(matches!(start(Some(&link)), Handoff::Delivered));
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), link);
        assert!(matches!(start(Some(&link)), Handoff::Delivered));
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), link);
        assert!(send(&pipe_name(), "tinyclient://join#invalid").is_err());
        assert!(receiver.try_recv().is_err());
    }
}
