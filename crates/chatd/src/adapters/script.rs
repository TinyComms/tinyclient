//! How a test steers a scripted wire (the Signal and Telegram fakes): it writes lines to a
//! control file in the scratch directory. On Unix that is a FIFO, which blocks the fake between
//! lines and gives each line once. Windows has no FIFO: the test appends to a plain file, the
//! fake reads on from where it stopped, looking again every 20 ms, and after each line it writes
//! how far it has read to `NAME.taken` beside it, which the test waits on as a
//! write to a FIFO waits on its reader.

use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::Mutex;

/// The control file `name` in the scratch directory: a FIFO, made if it is not there; on
/// Windows, which has none, a plain file.
pub fn control(name: &str) -> PathBuf {
    let path = protocol::support_dir().join(name);
    if !path.exists() {
        #[cfg(unix)]
        if let Ok(c_path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
            // SAFETY: `c_path` is a valid C string for the length of the call.
            unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        }
        #[cfg(windows)]
        {
            let _ = std::fs::OpenOptions::new().create(true).write(true).open(&path);
        }
    }
    path
}

/// Where the fake says how far into a control file it has read and acted.
#[cfg(windows)]
fn taken(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".taken");
    PathBuf::from(name)
}

/// Hands `each` every line written to a control file, and `None` whenever the writing paused: a
/// writer closed the FIFO, or on Windows nothing more is in the file. Stops when `each` says
/// false or the file cannot be opened. Each line is taken once, as a FIFO gives it.
pub fn follow(path: &Path, mut each: impl FnMut(Option<&str>) -> bool) {
    #[cfg(unix)]
    loop {
        use std::io::BufRead;
        // Blocks until a writer opens it, so waiting costs nothing.
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if !each(Some(&line)) {
                return;
            }
        }
        if !each(None) {
            return;
        }
    }
    #[cfg(windows)]
    {
        use std::io::{Read, Seek, SeekFrom};
        // How far each file has been read, kept across readers as a FIFO keeps nothing it gave.
        static READ: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());
        let offset = |path: &Path| {
            let read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            read.iter().find(|(of, _)| of == path).map_or(0, |(_, at)| *at)
        };
        let took = |path: &Path, at: u64| {
            let mut read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            read.retain(|(of, _)| of != path);
            read.push((path.to_owned(), at));
        };
        loop {
            let Ok(mut file) = std::fs::File::open(path) else {
                return;
            };
            let mut at = offset(path);
            let mut text = String::new();
            if file.seek(SeekFrom::Start(at)).is_ok() {
                let _ = file.read_to_string(&mut text);
            }
            // A line still being written waits for the next look.
            for line in text.split_inclusive('\n').filter(|line| line.ends_with('\n')) {
                at += line.len() as u64;
                took(path, at);
                let go_on = each(Some(line.trim_end()));
                let _ = std::fs::write(taken(path), at.to_string());
                if !go_on {
                    return;
                }
            }
            if !each(None) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// Wakes a reader waiting on a control file with one last line. True if there was a reader to
/// wake, as far as can be told: on Windows the line is left in the file.
pub fn wake(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut file) = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(path) {
            return file.write_all(b"closed\n").is_ok();
        }
        false
    }
    #[cfg(windows)]
    {
        if let Ok(mut file) = std::fs::OpenOptions::new().write(true).append(true).open(path) {
            return file.write_all(b"closed\n").is_ok();
        }
        false
    }
}
