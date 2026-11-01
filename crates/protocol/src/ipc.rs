//! Local socket IPC for chatd: one filesystem path on Unix, the matching named pipe on Windows.

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, Listener, ListenerOptions, Stream};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Binds the chat socket at `path` (`chat_sock_path()`).
pub struct ChatListener(Listener);

/// One client connection to chatd.
pub struct ChatStream {
    stream: Stream,
    /// Named pipes have no timeouts of their own: a read waits for data, up to this, by looking.
    #[cfg(windows)]
    read_timeout: std::cell::Cell<Option<Duration>>,
}

impl ChatListener {
    /// Removes a stale socket file, then listens.
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(not(windows))]
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        let owned = socket_path(path);
        let name = owned.to_fs_name::<GenericFilePath>()?;
        let listener = ListenerOptions::new().name(name).create_sync()?;
        // Explicit, not whatever the umask leaves: only this user can connect. The folder above
        // is owner-only as well, so the socket is never reachable in the time between the two.
        #[cfg(not(windows))]
        crate::private::tighten_file(path);
        Ok(Self(listener))
    }

    pub fn accept(&self) -> io::Result<ChatStream> {
        Ok(ChatStream::new(self.0.accept()?))
    }
}

impl ChatStream {
    fn new(stream: Stream) -> Self {
        Self {
            stream,
            #[cfg(windows)]
            read_timeout: std::cell::Cell::new(None),
        }
    }

    pub fn connect(path: &Path) -> io::Result<Self> {
        let owned = socket_path(path);
        let name = owned.to_fs_name::<GenericFilePath>()?;
        Ok(Self::new(Stream::connect(name)?))
    }

    /// A read that waits longer fails with `WouldBlock` on Unix and `TimedOut` on Windows.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.read_timeout.set(timeout);
            Ok(())
        }
        #[cfg(not(windows))]
        self.stream.set_recv_timeout(timeout)
    }

    /// Windows does without: a frame is small, and chatd reads each one whole as it comes.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(windows)]
        {
            let _ = timeout;
            Ok(())
        }
        #[cfg(not(windows))]
        self.stream.set_send_timeout(timeout)
    }

    /// Waits until the pipe holds something to read, it broke, or `timeout` passed. Looking
    /// costs a system call; the pause between looks grows to 10 ms.
    #[cfg(windows)]
    fn wait_for_data(&self, timeout: Duration) -> io::Result<()> {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        let Stream::NamedPipe(pipe) = &self.stream;
        let handle = pipe.as_handle().as_raw_handle();
        let deadline = std::time::Instant::now() + timeout;
        let mut pause = Duration::from_millis(1);
        loop {
            let mut available = 0u32;
            // SAFETY: `handle` is this stream's open pipe; only the count of waiting bytes is asked.
            let looked = unsafe { PeekNamedPipe(handle, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut available, std::ptr::null_mut()) };
            // A pipe that broke is the read's to report.
            if looked == 0 || available > 0 {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "chatd did not answer in time"));
            }
            std::thread::sleep(pause.min(left));
            pause = (pause * 2).min(Duration::from_millis(10));
        }
    }
}

impl io::Read for ChatStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        if let Some(timeout) = self.read_timeout.get() {
            self.wait_for_data(timeout)?;
        }
        self.stream.read(buf)
    }
}

impl io::Write for ChatStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Unix keeps the filesystem socket path. Windows named pipes are `\\.\pipe\...`, so the logical
/// `chat.sock` path is hashed into a pipe name. The `PathBuf` has to outlive the `Name` borrow.
fn socket_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.as_os_str().hash(&mut hasher);
        PathBuf::from(format!(r"\\.\pipe\tinyclient-{:016x}", hasher.finish()))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_chat_socket_is_for_its_owner_alone_and_still_answers() {
        let dir = std::env::temp_dir().join(format!("protocol-ipc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::private::dir(&dir).unwrap();
        let path = dir.join(crate::CHAT_SOCK_NAME);
        let listener = ChatListener::bind(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let client = std::thread::spawn({
            let path = path.clone();
            move || ChatStream::connect(&path).map(drop)
        });
        assert!(listener.accept().is_ok());
        assert!(client.join().unwrap().is_ok());
        // A stale socket from a chatd that died is replaced, and the new one is owner-only too. A
        // chatd that dies runs no destructor, so the listener is forgotten, not dropped: dropping
        // it lets interprocess unlink the socket file (it reclaims the name), and there would be
        // no stale file left to replace.
        std::mem::forget(listener);
        assert!(path.exists(), "the stale socket is still there");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let _again = ChatListener::bind(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }
}
