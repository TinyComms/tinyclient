//! Files and folders only their owner reads, for what tinyclient keeps on disk: the database,
//! the media cache, the call identity key, the list of keys a person verified, `crash.log` and
//! the chat socket.
//!
//! On Unix a folder is mode `0700` and a file `0600`. On Windows nothing here changes a mode:
//! the files inherit the access list of the folder they are in, which for the profile's
//! `AppData` is the person and the administrators, and moving that list is not attempted. On
//! every platform a replaced file is written beside and renamed over, so a reader sees the old
//! file or the whole new one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Makes `path` and the folders above it that are missing, and makes `path` itself owner-only.
pub fn dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    tighten_dir(path);
    Ok(())
}

/// Owner-only, if the platform has such a thing and the folder is ours to change. A folder we
/// do not own is left as it is: that is not a reason to stop.
pub fn tighten_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// As [`tighten_dir`], for a file or a socket.
pub fn tighten_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// A file made empty or new, owner-only from the moment it exists. An older file keeps its
/// place, and is made owner-only too.
pub fn create(path: &Path) -> io::Result<File> {
    let file = options().write(true).create(true).truncate(true).open(path)?;
    tighten_file(path);
    Ok(file)
}

/// A file opened to be added to, made owner-only from the moment it exists.
pub fn append(path: &Path) -> io::Result<File> {
    let file = options().append(true).create(true).open(path)?;
    tighten_file(path);
    Ok(file)
}

/// The file at `path`, there if it was not, left as it was, and owner-only either way. For a
/// file another library opens (SQLite), so that it never exists with wider rights.
pub fn touch(path: &Path) -> io::Result<()> {
    options().write(true).create(true).truncate(false).open(path)?;
    tighten_file(path);
    Ok(())
}

/// Replaces the file at `path` with `bytes`, owner-only: written to a file of its own beside it,
/// flushed to disk, then renamed over it. A crash leaves the old file or the new one, and the
/// folder holds no other file afterwards.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{}", std::process::id()));
    let temp = std::path::PathBuf::from(temp);
    let _ = fs::remove_file(&temp);
    let written = options().write(true).create_new(true).open(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    match written.and_then(|()| fs::rename(&temp, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// The permission bits of `path`, on Unix.
#[cfg(all(unix, test))]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("protocol-private-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_replaced_file_is_whole_and_leaves_nothing_beside_it() {
        let dir = scratch("atomic");
        let path = dir.join("verified.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two, longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two, longer");
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["verified.json"], "no temporary file is left");
        // A folder that is not there is an error, and still leaves nothing behind.
        assert!(write_atomic(&dir.join("missing").join("x"), b"x").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn create_append_and_touch_keep_what_is_there_as_each_says() {
        let dir = scratch("kinds");
        let path = dir.join("a");
        create(&path).unwrap().write_all(b"first").unwrap();
        append(&path).unwrap().write_all(b" more").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first more");
        touch(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first more", "touch leaves it as it is");
        touch(&dir.join("b")).unwrap();
        assert!(dir.join("b").exists());
        create(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"", "create starts it over");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn on_unix_only_the_owner_can_read_what_is_made_and_a_wider_one_is_narrowed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("modes");
        let folder = dir.join("support").join("e2ee");
        super::dir(&folder).unwrap();
        assert_eq!(mode(&folder), 0o700);
        // A folder an older build made wide is narrowed.
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        super::dir(&folder).unwrap();
        assert_eq!(mode(&folder), 0o700);
        for (name, make) in [
            ("create", (|p: &Path| create(p).map(drop)) as fn(&Path) -> io::Result<()>),
            ("append", |p| append(p).map(drop)),
            ("touch", touch),
            ("atomic", |p| write_atomic(p, b"x")),
        ] {
            let path = folder.join(name);
            make(&path).unwrap();
            assert_eq!(mode(&path), 0o600, "{name}");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            make(&path).unwrap();
            assert_eq!(mode(&path), 0o600, "{name} narrows an older, wider file");
        }
        let _ = fs::remove_dir_all(dir);
    }
}
