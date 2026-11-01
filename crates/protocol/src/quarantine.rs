//! The operating system's mark on a file that came from the internet, set on every attachment the
//! person saves or opens. With it, Windows SmartScreen and macOS Gatekeeper check the file when it
//! is opened, which is the scanning a server can no longer do for an end-to-end encrypted file.
//!
//! - Windows: the `Zone.Identifier` stream with `ZoneId=3` (Mark of the Web), as a browser writes.
//! - macOS: the `com.apple.quarantine` extended attribute.
//! - Linux has no such mark; this does nothing there.

use std::path::Path;

/// Marks `path` as downloaded. A failure is returned, never fatal to the save.
pub fn mark(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n")
    }
    #[cfg(target_os = "macos")]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        // 0081: downloaded, and not yet opened; then the time, the app, and no event id.
        let value = format!("0081;{now:08x};tinyclient;");
        let status = std::process::Command::new("/usr/bin/xattr")
            .arg("-w")
            .arg("com.apple.quarantine")
            .arg(value)
            .arg(path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("xattr refused"))
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_saved_file_carries_the_mark() {
        let path =
            std::env::temp_dir().join(format!("tinyclient-quarantine-{}.bin", std::process::id()));
        std::fs::write(&path, b"MZ").unwrap();
        super::mark(&path).unwrap();
        #[cfg(windows)]
        {
            let mut stream = path.as_os_str().to_owned();
            stream.push(":Zone.Identifier");
            assert!(std::fs::read_to_string(stream)
                .unwrap()
                .contains("ZoneId=3"));
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"MZ",
            "the file itself is unchanged"
        );
        let _ = std::fs::remove_file(path);
    }
}
