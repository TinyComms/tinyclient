//! Local socket IPC for chatd: one filesystem path on Unix, the matching named pipe on Windows.

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, Listener, ListenerOptions, Stream};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Binds the chat socket at `path` (`chat_sock_path()`).
pub struct ChatListener(Listener);

/// One client connection to chatd.
pub struct ChatStream(Stream);

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
        Ok(Self(listener))
    }

    pub fn accept(&self) -> io::Result<ChatStream> {
        Ok(ChatStream(self.0.accept()?))
    }
}

impl ChatStream {
    pub fn connect(path: &Path) -> io::Result<Self> {
        let owned = socket_path(path);
        let name = owned.to_fs_name::<GenericFilePath>()?;
        Ok(Self(Stream::connect(name)?))
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_recv_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_send_timeout(timeout)
    }
}

impl io::Read for ChatStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl io::Write for ChatStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
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
