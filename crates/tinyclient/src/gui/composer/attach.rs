//! Files on their way into a message: what the tray holds, why one is refused, and the files
//! the shell itself writes (pasted pixels, HEIC turned JPEG) in `outbox/`. Staging reads
//! nothing but the file's size and kind: bytes never leave disk until chatd streams them.
//! All plain functions over values, so the rules are tested without a window.

use protocol::{support_dir, Capabilities, Network, OutgoingFile, ATTACHMENTS_LIMIT};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Pasted and converted files older than this are swept at start: their send settled long ago.
pub const OUTBOX_KEEP: Duration = Duration::from_secs(3 * 24 * 3_600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Text,
    Other,
}

impl Kind {
    pub fn of(name: &str) -> Kind {
        let extension = name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).unwrap_or_default();
        match extension.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff" => Kind::Image,
            "mov" | "mp4" | "m4v" | "webm" | "mkv" | "avi" => Kind::Video,
            "mp3" | "m4a" | "wav" | "aac" | "ogg" | "opus" | "flac" | "aiff" => Kind::Audio,
            "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "dmg" | "xz" | "bz2" => Kind::Archive,
            "rs" | "py" | "js" | "ts" | "go" | "c" | "h" | "cpp" | "swift" | "java" | "json" | "toml" | "yaml" | "yml" | "sh"
            | "html" | "css" => Kind::Code,
            "pdf" | "txt" | "md" | "doc" | "docx" | "rtf" | "pages" | "csv" | "xls" | "xlsx" | "key" | "ppt" | "pptx" => Kind::Text,
            _ => Kind::Other,
        }
    }

    /// The short type word under a file chip's name: "PDF", "ZIP".
    pub fn word(name: &str) -> String {
        name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_uppercase()).filter(|word| word.len() <= 5).unwrap_or_else(|| "File".to_owned())
    }

    pub fn is_heic(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".heic") || lower.ends_with(".heif")
    }
}

/// What the disk says about a path when it is staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    File(u64),
    Folder,
    Missing,
}

pub fn look(path: &Path) -> Found {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Found::Folder,
        Ok(meta) => Found::File(meta.len()),
        Err(_) => Found::Missing,
    }
}

/// One chip in the tray.
#[derive(Debug, Clone, PartialEq)]
pub struct Staged {
    /// Tells chips apart for drawing and removing.
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub kind: Kind,
    /// Why it cannot go, in words for the chip's second line.
    pub refused: Option<String>,
    pub spoiler: bool,
    pub as_file: bool,
    /// A HEIC: the JPEG made of it once it is ready, and whether the user asked for the
    /// original instead.
    pub converted: Option<PathBuf>,
    pub converting: bool,
    pub original: bool,
    /// The shell wrote `path` (a paste) and deletes it once the message is settled.
    pub owned: bool,
}

impl Staged {
    pub fn new(id: u64, path: PathBuf, found: Found, can: &Capabilities, network: Network) -> Staged {
        let name = path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |name| name.to_string_lossy().into_owned());
        let bytes = match found {
            Found::File(bytes) => bytes,
            _ => 0,
        };
        let refused = refusal(found, can, network);
        let converting = refused.is_none() && Kind::is_heic(&name);
        Staged {
            id,
            kind: if found == Found::Folder { Kind::Other } else { Kind::of(&name) },
            path,
            name,
            bytes,
            refused,
            spoiler: false,
            as_file: false,
            converted: None,
            converting,
            original: false,
            owned: false,
        }
    }

    /// Shown as a picture rather than a file tile.
    pub fn pictured(&self) -> bool {
        self.refused.is_none() && matches!(self.kind, Kind::Image)
    }

    /// The JPEG stands in for a HEIC until "Send original" is checked.
    pub fn sends_jpeg(&self) -> bool {
        self.converted.is_some() && !self.original
    }

    /// What goes on the wire. `None` while it cannot go.
    pub fn outgoing(&self) -> Option<OutgoingFile> {
        if self.refused.is_some() || self.converting {
            return None;
        }
        let (path, name) = match (&self.converted, self.original) {
            (Some(jpeg), false) => {
                let stem = self.name.rsplit_once('.').map_or(self.name.as_str(), |(stem, _)| stem);
                (jpeg.clone(), format!("{stem}.jpg"))
            }
            _ => (self.path.clone(), self.name.clone()),
        };
        Some(OutgoingFile {
            path: path.to_string_lossy().into_owned(),
            name,
            spoiler: self.spoiler,
            as_file: self.as_file,
            original: self.converted.is_some() && self.original,
        })
    }

    /// Files the shell made for this chip, to delete when it is settled or removed.
    pub fn made(&self) -> Vec<PathBuf> {
        let mut made: Vec<PathBuf> = self.converted.iter().cloned().collect();
        if self.owned {
            made.push(self.path.clone());
        }
        made
    }
}

/// Why a path cannot be attached, checked against what the account may send before anything
/// is asked of the network.
pub fn refusal(found: Found, can: &Capabilities, network: Network) -> Option<String> {
    let name = network.label();
    match found {
        Found::Folder => Some("Folders can't be sent".to_owned()),
        Found::Missing => Some("Not on disk anymore".to_owned()),
        Found::File(_) if can.max_file_bytes == 0 => Some(format!("{name} can't take files here")),
        Found::File(bytes) if bytes > can.max_file_bytes => Some(format!("Too big for {name} · limit {}", size(can.max_file_bytes))),
        Found::File(_) => None,
    }
}

/// Most chips in one message: the account's limit, never more than the protocol carries.
pub fn most(can: &Capabilities) -> usize {
    (can.max_files.max(1) as usize).min(ATTACHMENTS_LIMIT)
}

/// Adds what fits and says how many did not.
pub fn admit(tray: &mut Vec<Staged>, incoming: Vec<Staged>, can: &Capabilities) -> usize {
    let room = most(can).saturating_sub(tray.len());
    let left = incoming.len().saturating_sub(room);
    tray.extend(incoming.into_iter().take(room));
    left
}

/// The caption when a full tray turned files away.
pub fn overflow_words(network: Network, can: &Capabilities, left: usize) -> String {
    let name = network.label();
    let files = if left == 1 { "1 was".to_owned() } else { format!("{left} were") };
    format!("{name} sends up to {} files at once. {files} not added.", most(can))
}

/// "2.4 MB", "820 KB", "20 MB".
pub fn size(bytes: u64) -> String {
    const KB: f64 = 1_000.;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        trimmed(b / (KB * KB * KB), "GB")
    } else if b >= KB * KB {
        trimmed(b / (KB * KB), "MB")
    } else if b >= KB {
        format!("{} KB", (b / KB).round() as u64)
    } else {
        format!("{bytes} bytes")
    }
}

fn trimmed(value: f64, unit: &str) -> String {
    if value >= 10. || (value.fract() * 10.).round() == 0. {
        format!("{} {unit}", value.round() as u64)
    } else {
        format!("{value:.1} {unit}")
    }
}

/// Why Return does nothing with this tray, naming the first chip in the way.
pub fn blocker(tray: &[Staged]) -> Option<String> {
    if let Some(refused) = tray.iter().find(|chip| chip.refused.is_some()) {
        return Some(match refused.bytes {
            0 => format!("Remove {} to send.", refused.name),
            bytes => format!("Remove {} to send. It is {}.", refused.name, size(bytes)),
        });
    }
    tray.iter().find(|chip| chip.converting).map(|chip| format!("{} is still being made into a JPEG.", chip.name))
}

/// What the tray's caption line adds about how the files will go, where the network splits
/// them or puts the text apart.
pub fn sending_note(network: Network, can: &Capabilities, tray: &[Staged], text: &str) -> Option<String> {
    if tray.is_empty() {
        return None;
    }
    if network == Network::Telegram {
        let media = tray.iter().filter(|chip| matches!(chip.kind, Kind::Image | Kind::Video) && !chip.as_file).count();
        let rest = tray.len() - media;
        if media > 0 && rest > 0 {
            return Some("Sends as 2 messages: photos, then files".to_owned());
        }
    }
    let limit = can.caption_limit as usize;
    (limit > 0 && text.trim().chars().count() > limit).then(|| "The text goes as its own message first.".to_owned())
}

/// Per-chip options this network has for this chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub spoiler: bool,
    pub as_file: bool,
    pub original: bool,
}

impl Options {
    pub fn any(&self) -> bool {
        self.spoiler || self.as_file || self.original
    }
}

pub fn options(chip: &Staged, can: &Capabilities, network: Network) -> Options {
    if chip.refused.is_some() {
        return Options::default();
    }
    let media = matches!(chip.kind, Kind::Image | Kind::Video);
    Options {
        // Telegram blurs photos and videos only; Discord any file.
        spoiler: can.spoiler && (network != Network::Telegram || media),
        as_file: can.as_file && media,
        original: chip.converted.is_some(),
    }
}

// The outbox.

pub fn outbox() -> PathBuf {
    support_dir().join("outbox")
}

/// "Pasted image 9.41.12.png", as macOS names a screenshot: the local time, to the second.
pub fn pasted_name(hour: u32, minute: u32, second: u32, extension: &str) -> String {
    let hour = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    format!("Pasted image {hour}.{minute:02}.{second:02}.{extension}")
}

/// A path in `dir` for `name` that nothing holds yet: "x.png", then "x 2.png".
pub fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = name.rsplit_once('.').map_or((name, None), |(stem, extension)| (stem, Some(extension)));
    (2..)
        .map(|n| match extension {
            Some(extension) => dir.join(format!("{stem} {n}.{extension}")),
            None => dir.join(format!("{stem} {n}")),
        })
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Writes pasted pixels as a file. Called off the main thread.
pub fn write_pasted(dir: &Path, bytes: &[u8], extension: &str, now: SystemTime) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let (hour, minute, second) = local_clock(now);
    let path = free_path(dir, &pasted_name(hour, minute, second, extension));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Turns a HEIC into a JPEG beside the outbox, with the system's own converter (ImageIO, via
/// `sips`). Called off the main thread. `None` when the system could not read it.
pub fn heic_to_jpeg(dir: &Path, heic: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let stem = heic.file_stem()?.to_string_lossy().into_owned();
    let out = free_path(dir, &format!("{stem}.jpg"));
    let done = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg"])
        .arg(heic)
        .arg("--out")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    (done.success() && out.exists()).then_some(out)
}

/// Deletes files the shell made. Only ever inside the outbox: a user's own file is never
/// touched, whatever a chip says.
pub fn delete_made(dir: &Path, paths: &[PathBuf]) {
    for path in paths {
        if path.parent() == Some(dir) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Removes what earlier runs left: files older than `keep`.
pub fn sweep(dir: &Path, keep: Duration, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut gone = 0;
    for entry in entries.flatten() {
        let old = entry.metadata().ok().and_then(|meta| meta.modified().ok()).and_then(|at| now.duration_since(at).ok()).is_some_and(|age| age > keep);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            gone += 1;
        }
    }
    gone
}

fn local_clock(now: SystemTime) -> (u32, u32, u32) {
    let seconds = now.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes a full `tm` into the buffer we own.
    unsafe {
        libc::localtime_r(&seconds, &mut tm);
    }
    (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discord() -> Capabilities {
        Capabilities { max_file_bytes: 20_000_000, max_files: 10, spoiler: true, ..Capabilities::default() }
    }

    fn telegram() -> Capabilities {
        Capabilities { max_file_bytes: 2 << 30, max_files: 10, caption_limit: 1_024, spoiler: true, as_file: true, ..Capabilities::default() }
    }

    fn chip(id: u64, name: &str, found: Found, can: &Capabilities, network: Network) -> Staged {
        Staged::new(id, PathBuf::from(format!("/Users/me/{name}")), found, can, network)
    }

    #[test]
    fn a_file_over_the_limit_is_refused_on_its_chip_before_anything_is_sent() {
        let can = discord();
        let big = chip(1, "clip.mov", Found::File(84_000_000), &can, Network::Discord);
        assert_eq!(big.refused.as_deref(), Some("Too big for Discord · limit 20 MB"));
        assert!(big.outgoing().is_none());
        let fits = chip(2, "photo.jpg", Found::File(20_000_000), &can, Network::Discord);
        assert!(fits.refused.is_none());
        assert_eq!(chip(3, "src", Found::Folder, &can, Network::Discord).refused.as_deref(), Some("Folders can't be sent"));
        assert_eq!(chip(4, "gone.pdf", Found::Missing, &can, Network::Discord).refused.as_deref(), Some("Not on disk anymore"));
        let none = Capabilities::default();
        assert_eq!(chip(5, "a.txt", Found::File(1), &none, Network::Teams).refused.as_deref(), Some("Teams can't take files here"));
        assert_eq!(blocker(&[fits.clone(), big]).as_deref(), Some("Remove clip.mov to send. It is 84 MB."));
        assert_eq!(blocker(&[fits]), None);
    }

    #[test]
    fn the_tray_takes_up_to_the_networks_count_and_says_how_many_were_left() {
        let can = Capabilities { max_files: 3, ..discord() };
        let mut tray = Vec::new();
        let files = |from: u64, count: u64| (from..from + count).map(|id| chip(id, &format!("{id}.png"), Found::File(10), &can, Network::Discord)).collect();
        assert_eq!(admit(&mut tray, files(0, 2), &can), 0);
        assert_eq!(admit(&mut tray, files(2, 3), &can), 2);
        assert_eq!(tray.len(), 3);
        assert_eq!(overflow_words(Network::Discord, &can, 1), "Discord sends up to 3 files at once. 1 was not added.");
        assert_eq!(overflow_words(Network::Discord, &can, 2), "Discord sends up to 3 files at once. 2 were not added.");
        // Never past what one message carries, whatever the account says.
        let lavish = Capabilities { max_files: 500, ..discord() };
        assert_eq!(most(&lavish), ATTACHMENTS_LIMIT);
        assert_eq!(most(&Capabilities::default()), 1);
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(size(20_000_000), "20 MB");
        assert_eq!(size(2_400_000), "2.4 MB");
        assert_eq!(size(84_200_000), "84 MB");
        assert_eq!(size(820_400), "820 KB");
        assert_eq!(size(1 << 30), "1.1 GB");
        assert_eq!(size(512), "512 bytes");
    }

    #[test]
    fn options_are_only_those_the_network_has_for_that_kind_of_file() {
        let photo = chip(1, "a.jpg", Found::File(1), &telegram(), Network::Telegram);
        let pdf = chip(2, "a.pdf", Found::File(1), &telegram(), Network::Telegram);
        assert_eq!(options(&photo, &telegram(), Network::Telegram), Options { spoiler: true, as_file: true, original: false });
        assert_eq!(options(&pdf, &telegram(), Network::Telegram), Options::default());
        let any = chip(3, "a.pdf", Found::File(1), &discord(), Network::Discord);
        assert_eq!(options(&any, &discord(), Network::Discord), Options { spoiler: true, ..Options::default() });
        assert!(!options(&photo, &Capabilities::default(), Network::Slack).any());
    }

    #[test]
    fn a_heic_sends_its_jpeg_until_the_original_is_asked_for() {
        let mut heic = chip(1, "IMG_0042.HEIC", Found::File(3_000_000), &discord(), Network::Discord);
        assert!(heic.converting);
        assert!(heic.outgoing().is_none());
        assert_eq!(blocker(std::slice::from_ref(&heic)).as_deref(), Some("IMG_0042.HEIC is still being made into a JPEG."));
        heic.converting = false;
        heic.converted = Some(PathBuf::from("/out/IMG_0042.jpg"));
        let sent = heic.outgoing().expect("sendable");
        assert_eq!((sent.path.as_str(), sent.name.as_str(), sent.original), ("/out/IMG_0042.jpg", "IMG_0042.jpg", false));
        heic.original = true;
        let sent = heic.outgoing().expect("sendable");
        assert_eq!((sent.path.as_str(), sent.original), ("/Users/me/IMG_0042.HEIC", true));
        assert_eq!(heic.made(), [PathBuf::from("/out/IMG_0042.jpg")]);
    }

    #[test]
    fn telegram_says_when_it_splits_and_when_the_text_goes_apart() {
        let can = telegram();
        let photo = chip(1, "a.jpg", Found::File(1), &can, Network::Telegram);
        let pdf = chip(2, "b.pdf", Found::File(1), &can, Network::Telegram);
        assert_eq!(sending_note(Network::Telegram, &can, &[photo.clone(), pdf], "hi").as_deref(), Some("Sends as 2 messages: photos, then files"));
        assert_eq!(sending_note(Network::Telegram, &can, std::slice::from_ref(&photo), "hi"), None);
        let long = "a".repeat(1_100);
        assert_eq!(sending_note(Network::Telegram, &can, &[photo], &long).as_deref(), Some("The text goes as its own message first."));
        assert_eq!(sending_note(Network::Telegram, &can, &[], &long), None);
    }

    #[test]
    fn pasted_pictures_are_named_like_screenshots_and_never_overwrite() {
        assert_eq!(pasted_name(21, 41, 12, "png"), "Pasted image 9.41.12.png");
        assert_eq!(pasted_name(0, 5, 9, "jpeg"), "Pasted image 12.05.09.jpeg");
        let dir = std::env::temp_dir().join(format!("tc-outbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = SystemTime::now();
        let one = write_pasted(&dir, b"one", "png", now).expect("written");
        let two = write_pasted(&dir, b"two", "png", now).expect("written");
        assert_ne!(one, two);
        assert!(two.file_name().unwrap().to_string_lossy().ends_with(" 2.png"));
        assert_eq!(std::fs::read(&one).unwrap(), b"one");

        // Only the outbox's own files are ever deleted.
        let outside = std::env::temp_dir().join(format!("tc-user-{}.png", std::process::id()));
        std::fs::write(&outside, b"mine").unwrap();
        delete_made(&dir, &[one.clone(), outside.clone()]);
        assert!(!one.exists() && outside.exists());
        let _ = std::fs::remove_file(&outside);

        // The sweep takes what is older than it keeps, and nothing newer.
        assert_eq!(sweep(&dir, OUTBOX_KEEP, now), 0);
        assert_eq!(sweep(&dir, Duration::ZERO, now + Duration::from_secs(5)), 1);
        assert!(!two.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kinds_follow_the_extension_whatever_its_case() {
        assert_eq!(Kind::of("A.JPG"), Kind::Image);
        assert_eq!(Kind::of("clip.mov"), Kind::Video);
        assert_eq!(Kind::of("notes"), Kind::Other);
        assert_eq!(Kind::word("trail.pdf"), "PDF");
        assert_eq!(Kind::word("README"), "File");
        assert!(Kind::is_heic("x.heif") && !Kind::is_heic("x.jpg"));
    }
}
