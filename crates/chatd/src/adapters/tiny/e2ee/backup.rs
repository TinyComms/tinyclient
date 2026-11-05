//! The encrypted backup file (`e2ee::backup`): what this device's cache opened and the safety
//! numbers the person verified, in a file under a key from a passphrase, saved where they choose;
//! and taken back in on a device of the same person. No server keeps a backup, ever. Importing
//! asks the server which conversations the person is in, and nothing else.

use super::linking::{pack, Merging};
use super::{lock, private_dir, Shared};
use e2ee::archive::{self, Meta};
use e2ee::backup::{Header, Params, HEADER_LEN};
use e2ee::link::Scope;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The shortest passphrase a backup is made with.
pub(super) const SHORTEST: usize = 12;

/// Writes the backup to `path` (through a temp file beside it, moved over at the end). Answers
/// how many messages it holds.
pub(super) fn export(device: &Shared, path: &Path, passphrase: &str) -> Result<u64, String> {
    export_with(device, path, passphrase, Params::default())
}

pub(super) fn export_with(
    device: &Shared,
    path: &Path,
    passphrase: &str,
    params: Params,
) -> Result<u64, String> {
    if passphrase.chars().count() < SHORTEST {
        return Err(format!(
            "Choose a passphrase of at least {SHORTEST} characters."
        ));
    }
    let (dir, cache_key, verified, user, from_device) = {
        let device = lock(device);
        (
            device.dir.clone(),
            device
                .cache_key
                .clone()
                .ok_or("Encryption isn't set up on this device.")?,
            device.notes.verified.clone(),
            device.saved.user_id.clone(),
            device
                .engine
                .as_ref()
                .map(|engine| engine.device_id().to_string())
                .unwrap_or_default(),
        )
    };
    let header = Header::new(params).map_err(|_| "No randomness is available.".to_owned())?;
    let key = header
        .key(passphrase)
        .map_err(|_| "The backup couldn't be locked.".to_owned())?;
    let meta = Meta {
        v: archive::VERSION,
        user,
        from_device,
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
            .to_string(),
        scope: Scope::All,
    };
    let folder = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let failed = |err: std::io::Error| format!("The backup couldn't be saved there: {err}");
    let mut temp = tempfile::NamedTempFile::new_in(folder).map_err(failed)?;
    let summary = {
        let mut out = std::io::BufWriter::new(temp.as_file_mut());
        out.write_all(&header.to_bytes()).map_err(failed)?;
        let (mut out, summary, _) = pack(
            out,
            &key,
            &header.to_bytes(),
            &dir,
            &cache_key,
            &verified,
            u64::MAX,
            &meta,
        )
        .map_err(|err| format!("The backup couldn't be written: {err}"))?;
        out.flush().map_err(failed)?;
        summary
    };
    temp.as_file().sync_all().map_err(failed)?;
    temp.persist(path).map_err(|err| failed(err.error))?;
    Ok(summary.msgs)
}

/// Takes the backup at `path` in: only what this device does not have, only conversations the
/// person is in (`members`), never another person's. Answers how many messages were added.
pub(super) fn import(
    device: &Shared,
    path: &Path,
    passphrase: &str,
    members: std::collections::BTreeSet<String>,
) -> Result<u64, String> {
    let unreadable = "That file isn't a tinyclient backup, or it was changed.";
    let mut file =
        std::fs::File::open(path).map_err(|err| format!("The backup couldn't be opened: {err}"))?;
    let mut first = vec![0u8; HEADER_LEN];
    file.read_exact(&mut first)
        .map_err(|_| unreadable.to_owned())?;
    let header = Header::parse(&first).map_err(|_| unreadable.to_owned())?;
    // Argon2 takes a second or so: not under the device's lock.
    let key = header.key(passphrase).map_err(|_| unreadable.to_owned())?;
    let user = lock(device).saved.user_id.clone();
    let open = || {
        let mut file = std::fs::File::open(path).map_err(|err| e2ee::Error::Io(err.to_string()))?;
        file.seek(SeekFrom::Start(HEADER_LEN as u64))
            .map_err(|err| e2ee::Error::Io(err.to_string()))?;
        Ok(std::io::BufReader::new(file))
    };
    let mut device = lock(device);
    if device.cache_key.is_none() {
        return Err("Encryption isn't set up on this device.".to_owned());
    }
    private_dir(&device.dir)
        .map_err(|err| format!("The encryption folder could not be made: {err}"))?;
    let mut into = Merging {
        device: &mut device,
        members,
        from: String::new(),
    };
    let merged = match archive::merge(&key, &first, &user, open, &mut into) {
        Ok((_, merged)) => merged,
        Err(e2ee::Error::Malformed("archive: another person's")) => {
            return Err("That backup is of another account.".to_owned())
        }
        // The wrong passphrase gives another key: the first chunk does not open.
        Err(e2ee::Error::Authentication) => {
            return Err("That passphrase doesn't open this backup.".to_owned())
        }
        Err(_) => return Err(unreadable.to_owned()),
    };
    into.device.save_notes();
    Ok(merged.added)
}

#[cfg(test)]
mod tests {
    use super::super::{Device, Opened, Saved};
    use super::*;
    use e2ee::store::CacheKey;
    use std::sync::{Arc, Mutex};

    fn cheap() -> Params {
        Params {
            memory_kib: 64,
            passes: 1,
            lanes: 1,
        }
    }

    fn device(name: &str, user: &str) -> Shared {
        let saved = Saved {
            base_url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            user_id: user.into(),
            email: None,
            display_name: String::new(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir =
            std::env::temp_dir().join(format!("chatd-e2ee-backup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&device.dir);
        private_dir(&device.dir).unwrap();
        device.cache_key = Some(CacheKey::generate().unwrap());
        Arc::new(Mutex::new(device))
    }

    const ADA: &str = "7e57ada0-0000-4000-8000-0000000000b1";

    #[test]
    fn a_backup_goes_to_a_fresh_device_of_the_same_person_with_its_passphrase() {
        let old = device("old", ADA);
        {
            let mut old = lock(&old);
            old.remember(
                "c1",
                "m1",
                Opened {
                    text: "first".into(),
                    at: 1,
                    ..Opened::default()
                },
            );
            old.remember(
                "c2",
                "m2",
                Opened {
                    text: "elsewhere".into(),
                    at: 2,
                    ..Opened::default()
                },
            );
            old.notes.verified.insert("bea".into(), "123".into());
        }
        let path = std::env::temp_dir().join(format!("chatd-backup-{}.tnyb", std::process::id()));
        assert!(export_with(&old, &path, "short", cheap())
            .unwrap_err()
            .contains("at least 12"));
        assert_eq!(
            export_with(&old, &path, "a long passphrase", cheap()),
            Ok(2)
        );
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(5).any(|w| w == b"first"),
            "nothing in the clear"
        );

        let fresh = device("fresh", ADA);
        let members: std::collections::BTreeSet<String> = ["c1".to_owned()].into();
        assert!(import(&fresh, &path, "a long passphrase?", members.clone())
            .unwrap_err()
            .contains("passphrase"));
        assert_eq!(
            import(&fresh, &path, "a long passphrase", members.clone()),
            Ok(1)
        );
        assert_eq!(
            import(&fresh, &path, "a long passphrase", members),
            Ok(0),
            "a second import adds nothing"
        );
        let mut fresh = lock(&fresh);
        assert_eq!(
            fresh
                .cache("c1")
                .and_then(|cache| cache.rows.get("m1").map(|o| o.text.clone()))
                .as_deref(),
            Some("first")
        );
        assert!(
            fresh.cache("c2").is_none_or(|cache| cache.rows.is_empty()),
            "not a conversation of theirs now"
        );
        assert_eq!(
            fresh.notes.verified.get("bea").map(String::as_str),
            Some("123")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn another_persons_backup_and_a_file_that_is_not_one_are_refused() {
        let theirs = device("theirs", "7e57bea0-0000-4000-8000-0000000000b2");
        lock(&theirs).remember(
            "c1",
            "m1",
            Opened {
                text: "x".into(),
                ..Opened::default()
            },
        );
        let path =
            std::env::temp_dir().join(format!("chatd-backup-other-{}.tnyb", std::process::id()));
        export_with(&theirs, &path, "their long passphrase", cheap()).unwrap();
        let mine = device("mine", ADA);
        let members: std::collections::BTreeSet<String> = ["c1".to_owned()].into();
        assert!(
            import(&mine, &path, "their long passphrase", members.clone())
                .unwrap_err()
                .contains("another account")
        );
        std::fs::write(&path, b"not a backup at all").unwrap();
        assert!(import(&mine, &path, "their long passphrase", members)
            .unwrap_err()
            .contains("isn't a tinyclient backup"));
        let _ = std::fs::remove_file(&path);
    }
}
