//! How a test steers a scripted wire (the Signal and Telegram fakes): it appends lines to a
//! control file in the scratch directory. The fake reads on from where it stopped, looking again
//! every 20 ms, and after each line it writes how far it has read to `NAME.taken` beside it. The
//! test waits on that, so a line has been acted on when its write returns.
//!
//! A FIFO did this once, and lost lines: a line written as the fake met the end of the last
//! writer's lines went into a pipe the fake then closed, and a closed pipe drops what it holds.
//! Windows has no FIFO at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often a fake looks for a new line.
const LOOK: Duration = Duration::from_millis(20);

/// The control file `name` in the scratch directory, made empty if it is not there.
pub fn control(name: &str) -> PathBuf {
    let path = protocol::support_dir().join(name);
    if !path.exists() {
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    }
    path
}

/// Where the fake says how far into a control file it has read and acted, in bytes.
fn taken(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".taken");
    PathBuf::from(name)
}

/// Hands `each` every line written to a control file, each once, oldest first. Before each line,
/// and every 20 ms while none comes, `each` is asked with `None` whether to go on: false stops
/// it, and no line is taken after that. Stops too if the file cannot be read.
pub fn follow(path: &Path, mut each: impl FnMut(Option<&str>) -> bool) {
    // Kept beside the file, so a fake that comes after this one goes on from here.
    let mut at: u64 = std::fs::read_to_string(taken(path)).ok().and_then(|at| at.trim().parse().ok()).unwrap_or(0);
    loop {
        if !each(None) {
            return;
        }
        let Ok(text) = std::fs::read(path) else {
            return;
        };
        let fresh = text.get(at as usize..).unwrap_or_default();
        // A line still being written waits for the next look.
        let whole = fresh.iter().rposition(|byte| *byte == b'\n').map_or(0, |end| end + 1);
        if whole == 0 {
            std::thread::sleep(LOOK);
            continue;
        }
        for line in fresh[..whole].split_inclusive(|byte| *byte == b'\n') {
            if !each(None) {
                return;
            }
            at += line.len() as u64;
            let _ = std::fs::write(taken(path), at.to_string());
            if !each(Some(String::from_utf8_lossy(line).trim_end())) {
                return;
            }
        }
    }
}
