//! The one upload helper every adapter's `SendWith` uses. Bytes go from the file on disk to the
//! network through one fixed buffer (or one part, for networks that take parts); the whole file
//! is never in memory, and chatd never decodes it.
//!
//! // CONTRACT: declared from `blobs.rs` (`crate::blobs::upload`) because `lib.rs` is frozen. The
//! // integrator moves it to `pub mod upload;` in `lib.rs`; `blobs.rs` keeps `pub use` so callers
//! // need not change.
//!
//! # How an adapter uses it
//!
//! ```text
//! Command::SendWith { conversation_id, client_id, text, reply_to, with } =>
//!     upload::spawn(&ctx, &conversation_id, &client_id, &with.files, move |up| {
//!         for at in 0..up.files().len() {
//!             let (url, headers) = ask_the_network_where(up.files()[at].name, up.files()[at].bytes)?;
//!             up.send(at, &Target::put(url).headers(headers))?;   // or send_form, send_range, parts
//!         }
//!         post_the_message_naming_the_uploads()                  // -> Result<Message, String>
//!     }),
//! Command::CancelSend { client_id, .. } => upload::cancel(&ctx.account, &client_id),
//! ```
//!
//! - [`spawn`]: runs one send on its own thread so the adapter keeps serving commands (a
//!   `CancelSend` above all). Reads the files ([`Upload::new`]), runs `work`, answers through
//!   `Context::sent`, and removes the shell's `outbox/` copies once the message went.
//! - [`cancel`]: the user pressed Cancel. The upload stops at the next buffer or part, the
//!   request body is cut short so the network never sees a whole request, and the send is
//!   answered "Cancelled" ([`CANCELLED`]). A cancel that arrives before the upload began holds.
//! - [`Upload::new`]: the files a `SendWith` names, opened and checked ([`allowed`], the same rule
//!   the server's `Send` pre-check refuses by). A file that cannot be read is refused here, named.
//! - [`Upload::files`]: each file's shown `name`, `bytes`, `mime` and per-file options.
//! - [`Upload::send`]: one file as the whole body of a `PUT` or `POST` (Discord's attachment
//!   URL, Slack's `files.getUploadURLExternal` URL, a OneDrive simple upload).
//! - [`Upload::send_form`]: one file as a `multipart/form-data` field, with text fields before it.
//! - [`Upload::send_range`]: a byte range of one file with `Content-Range` (a OneDrive upload
//!   session, which takes ranges in multiples of 320 KiB).
//! - [`Upload::parts`]: one file in parts of a fixed size, handed to a callback one at a time
//!   (Telegram's `saveFilePart` / `saveBigFilePart`, 512 KB). `from` resumes at a part.
//! - [`Upload::done`]: the message went; removes the `outbox/` copies. [`spawn`] calls it.
//! - [`discard`]: the user dropped a pending message; removes its `outbox/` copies.
//!
//! Every failure is a sentence that names the file ("cat.png could not be sent: the server
//! answered 500."), ready for `Context::sent`. Progress goes to `Context::upload` at most four
//! times a second and once at the end, never per buffer. Requests go only to the network's own
//! upload hosts ([`Target`] is checked, never redirected) and carry only the headers the adapter
//! names.
//!
//! Which files may be read: a regular file anywhere the user can pick one, or the shell's
//! `outbox/` (pasted pixels). Never chatd's own directory (the store, sessions, the media cache),
//! never `~/Library` or a hidden directory in the home directory (keychains, keys, other apps'
//! secrets), never a device, a folder or a symlink inside `outbox/`, and never a path whose real
//! place is any of those.

use crate::adapters::Context;
use protocol::{AccountKey, Message, Network, OutgoingFile};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a cancelled send is answered with.
pub const CANCELLED: &str = "Cancelled";
/// Largest part [`Upload::parts`] holds: the upload buffer's whole budget.
pub const LARGEST_PART: usize = 8 * 1024 * 1024;
/// Progress is told no more often than this, and once at the end.
const TOLD_EVERY: Duration = Duration::from_millis(250);
/// Most of a network's answer to an upload that is kept, for the adapter to read the id from.
const ANSWER_KEPT: u64 = 1024 * 1024;
/// Cancels that came before their upload began, kept until it does.
const EARLY_CANCELS: usize = 64;

/// One file of a send, opened.
pub struct Source {
    /// The name the network shows: the one the shell gave, else the file's own.
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// From the name's extension; `application/octet-stream` when unknown.
    pub mime: &'static str,
    pub spoiler: bool,
    pub as_file: bool,
    pub original: bool,
    /// The shell staged it in `outbox/`, so it goes once the message went.
    outbox: bool,
    file: File,
}

/// Where one request goes. Only the headers named here are sent, and only to a host the
/// network uploads to.
#[derive(Debug, Clone)]
pub struct Target {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Put,
    Post,
}

impl Target {
    pub fn put(url: impl Into<String>) -> Target {
        Target { method: Method::Put, url: url.into(), headers: Vec::new() }
    }

    pub fn post(url: impl Into<String>) -> Target {
        Target { method: Method::Post, url: url.into(), headers: Vec::new() }
    }

    pub fn header(mut self, name: &str, value: &str) -> Target {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn headers(mut self, headers: impl IntoIterator<Item = (String, String)>) -> Target {
        self.headers.extend(headers);
        self
    }
}

/// What the network answered a successful upload: a 2xx status and the start of its body.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: String,
}

/// One part handed to [`Upload::parts`]'s callback.
pub struct Part<'a> {
    /// From 0.
    pub index: u32,
    /// Parts in the whole file.
    pub count: u32,
    pub bytes: &'a [u8],
}

type Report = Box<dyn FnMut(u64, u64) + Send>;
type Halted = Box<dyn Fn() -> bool + Send>;

struct Meter {
    /// Bytes of each file streamed so far.
    sent: Vec<u64>,
    total: u64,
    told: Option<Instant>,
    report: Report,
    cancel: Arc<AtomicBool>,
    halted: Halted,
}

impl Meter {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || (self.halted)()
    }

    fn set(&mut self, at: usize, sent: u64) {
        self.sent[at] = sent;
        self.tell();
    }

    fn add(&mut self, at: usize, more: u64) {
        self.sent[at] += more;
        self.tell();
    }

    fn tell(&mut self) {
        let sent = self.sent.iter().sum::<u64>().min(self.total);
        let end = sent >= self.total;
        if end || self.told.is_none_or(|told| told.elapsed() >= TOLD_EVERY) {
            self.told = Some(Instant::now());
            (self.report)(sent, self.total);
        }
    }
}

/// One send's files on their way.
pub struct Upload {
    account: AccountKey,
    client_id: String,
    files: Vec<Source>,
    meter: Meter,
    agent: Option<ureq::Agent>,
}

impl Upload {
    /// The files of a `SendWith`, opened and checked. Progress goes to `Context::upload`; the
    /// adapter stopping counts as a cancel.
    pub fn new(ctx: &Context, conversation_id: &str, client_id: &str, files: &[OutgoingFile]) -> Result<Upload, String> {
        let (told, conversation, client) = (ctx.clone(), conversation_id.to_owned(), client_id.to_owned());
        let report = move |sent, total| told.upload(&conversation, &client, sent, total);
        let halted = ctx.clone();
        Upload::open(&ctx.account, client_id, files, Box::new(report), Box::new(move || halted.stopped()))
    }

    /// The same without a `Context`: progress goes to `report` as `(sent, total)`.
    pub fn with_progress(
        account: &AccountKey,
        client_id: &str,
        files: &[OutgoingFile],
        report: impl FnMut(u64, u64) + Send + 'static,
    ) -> Result<Upload, String> {
        Upload::open(account, client_id, files, Box::new(report), Box::new(|| false))
    }

    fn open(account: &AccountKey, client_id: &str, files: &[OutgoingFile], report: Report, halted: Halted) -> Result<Upload, String> {
        let mut sources = Vec::with_capacity(files.len());
        for staged in files {
            let name = shown(staged);
            let (path, outbox) = allowed(&staged.path).map_err(|why| format!("{name} {why}"))?;
            let unreadable = |err: std::io::Error| format!("{name} could not be read: {}.", plain(&err));
            let file = File::open(&path).map_err(unreadable)?;
            let meta = file.metadata().map_err(unreadable)?;
            if !meta.is_file() {
                return Err(format!("{name} is not a file. Folders can't be sent."));
            }
            sources.push(Source {
                mime: mime_of(&name),
                name,
                path,
                bytes: meta.len(),
                spoiler: staged.spoiler,
                as_file: staged.as_file,
                original: staged.original,
                outbox,
                file,
            });
        }
        let total = sources.iter().map(|source| source.bytes).sum();
        let cancel = register(account, client_id);
        Ok(Upload {
            account: account.clone(),
            client_id: client_id.to_owned(),
            meter: Meter {
                sent: vec![0; sources.len()],
                total,
                told: None,
                report,
                cancel,
                halted,
            },
            files: sources,
            agent: None,
        })
    }

    pub fn files(&self) -> &[Source] {
        &self.files
    }

    /// Bytes of all the files together.
    pub fn total(&self) -> u64 {
        self.meter.total
    }

    /// The user cancelled, or the adapter is stopping.
    pub fn cancelled(&self) -> bool {
        self.meter.stopped()
    }

    /// `Err(CANCELLED)` once cancelled. Check between steps of a flow that are not uploads
    /// (asking for an upload URL, posting the message).
    pub fn check(&self) -> Result<(), String> {
        match self.cancelled() {
            true => Err(CANCELLED.to_owned()),
            false => Ok(()),
        }
    }

    /// File `at` as the whole body, with its length.
    pub fn send(&mut self, at: usize, target: &Target) -> Result<Answer, String> {
        let bytes = self.files[at].bytes;
        let target = with_default(target, "Content-Type", self.files[at].mime);
        self.stream(at, &target, 0, bytes, Vec::new(), Vec::new())
    }

    /// `len` bytes of file `at` from `start`, with `Content-Range`.
    pub fn send_range(&mut self, at: usize, target: &Target, start: u64, len: u64) -> Result<Answer, String> {
        let bytes = self.files[at].bytes;
        let len = len.min(bytes.saturating_sub(start));
        let range = match len {
            0 => format!("bytes */{bytes}"),
            _ => format!("bytes {start}-{}/{bytes}", start + len - 1),
        };
        let target = target.clone().header("Content-Range", &range);
        self.stream(at, &target, start, len, Vec::new(), Vec::new())
    }

    /// File `at` as the form field `field`, after the text fields `fields`.
    pub fn send_form(&mut self, at: usize, target: &Target, field: &str, fields: &[(&str, &str)]) -> Result<Answer, String> {
        let boundary = format!("tinyclient-{:016x}", fnv(&format!("{}{}{at}", self.client_id, self.files[at].name)));
        let mut head = String::new();
        for (name, value) in fields {
            head.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n", quoted(name)));
        }
        let source = &self.files[at];
        head.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            quoted(field),
            quoted(&source.name),
            source.mime
        ));
        let tail = format!("\r\n--{boundary}--\r\n");
        let target = target.clone().header("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
        let bytes = source.bytes;
        self.stream(at, &target, 0, bytes, head.into_bytes(), tail.into_bytes())
    }

    /// File `at` in parts of `size` bytes (at most [`LARGEST_PART`]), from part `from`, each
    /// handed to `each` in order. One buffer of `size` is held, whatever the file's size. A
    /// cancel is honored between parts.
    pub fn parts(&mut self, at: usize, size: usize, from: u32, mut each: impl FnMut(Part) -> Result<(), String>) -> Result<(), String> {
        let size = size.clamp(1, LARGEST_PART);
        let name = self.files[at].name.clone();
        let bytes = self.files[at].bytes;
        let count = bytes.div_ceil(size as u64).max(1) as u32;
        let start = from as u64 * size as u64;
        let unreadable = |err: std::io::Error| format!("{name} could not be read: {}.", plain(&err));
        self.files[at].file.seek(SeekFrom::Start(start.min(bytes))).map_err(unreadable)?;
        self.meter.set(at, start.min(bytes));
        let mut buffer = vec![0u8; size];
        for index in from..count {
            self.check()?;
            let want = (bytes - index as u64 * size as u64).min(size as u64) as usize;
            let mut filled = 0;
            while filled < want {
                match self.files[at].file.read(&mut buffer[filled..want]) {
                    Ok(0) => return Err(format!("{name} changed while it was being sent.")),
                    Ok(read) => filled += read,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(unreadable(err)),
                }
            }
            each(Part { index, count, bytes: &buffer[..want] })?;
            self.meter.add(at, want as u64);
        }
        Ok(())
    }

    /// The message went: the shell's `outbox/` copies are not needed again.
    pub fn done(self) {
        for source in &self.files {
            if source.outbox {
                let _ = std::fs::remove_file(&source.path);
            }
        }
    }

    fn agent(&mut self) -> ureq::Agent {
        self.agent.get_or_insert_with(agent).clone()
    }

    /// One request whose body is `head`, `len` bytes of file `at` from `start`, and `tail`.
    fn stream(&mut self, at: usize, target: &Target, start: u64, len: u64, head: Vec<u8>, tail: Vec<u8>) -> Result<Answer, String> {
        self.check()?;
        let name = self.files[at].name.clone();
        let network = self.account.network;
        if !uploads_to(network, &target.url) {
            return Err(format!("{name} could not be sent: {} does not take files at that address.", network.label()));
        }
        let unreadable = |err: std::io::Error| format!("{name} could not be read: {}.", plain(&err));
        self.files[at].file.seek(SeekFrom::Start(start)).map_err(unreadable)?;
        self.meter.set(at, start);
        let length = head.len() as u64 + len + tail.len() as u64;
        let agent = self.agent();
        let Upload { files, meter, .. } = self;
        let mut feed = Feed {
            file: &mut files[at].file,
            at,
            left: len,
            shrank: false,
            meter,
        };
        let mut body = std::io::Cursor::new(head).chain(&mut feed).chain(std::io::Cursor::new(tail));
        let mut request = match target.method {
            Method::Put => agent.put(&target.url),
            Method::Post => agent.post(&target.url),
        };
        for (header, value) in &target.headers {
            request = request.header(header.as_str(), value.as_str());
        }
        let sent = request.header("Content-Length", &length.to_string()).send(ureq::SendBody::from_reader(&mut body));
        drop(body);
        let (shrank, stopped) = (feed.shrank, feed.meter.stopped());
        if stopped {
            return Err(CANCELLED.to_owned());
        }
        if shrank {
            return Err(format!("{name} changed while it was being sent."));
        }
        let mut response = sent.map_err(|err| format!("{name} could not be sent: {}.", crate::adapters::net::redact(&err.to_string())))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("{name} could not be sent: the server answered {status}."));
        }
        let body = response.body_mut().with_config().limit(ANSWER_KEPT).read_to_string().unwrap_or_default();
        Ok(Answer { status, body })
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        unregister(&self.account, &self.client_id);
    }
}

/// A file's bytes as a request body: counts progress, stops on cancel, and never hands over
/// more than `left` bytes.
struct Feed<'a> {
    file: &'a mut File,
    at: usize,
    left: u64,
    shrank: bool,
    meter: &'a mut Meter,
}

impl Read for Feed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        // An error, not an end: a body cut short must never look like a whole request.
        if self.meter.stopped() {
            return Err(std::io::Error::other(CANCELLED));
        }
        let most = buf.len().min(self.left as usize);
        let read = self.file.read(&mut buf[..most])?;
        if read == 0 {
            self.shrank = true;
            return Err(std::io::Error::other("the file got shorter"));
        }
        self.left -= read as u64;
        self.meter.add(self.at, read as u64);
        Ok(read)
    }
}

/// Runs one `SendWith` on its own thread: opens the files, runs `work`, and answers through
/// `Context::sent`. A cancel turns a failure into "Cancelled"; a message the network already
/// took stays sent. The `outbox/` copies go once it went.
pub fn spawn<F>(ctx: &Context, conversation_id: &str, client_id: &str, files: &[OutgoingFile], work: F)
where
    F: FnOnce(&mut Upload) -> Result<Message, String> + Send + 'static,
{
    let (ctx, conversation_id, client_id, files) = (ctx.clone(), conversation_id.to_owned(), client_id.to_owned(), files.to_vec());
    let answer = ctx.clone();
    let (conversation, client) = (conversation_id.clone(), client_id.clone());
    let spawned = std::thread::Builder::new().name("upload".into()).stack_size(512 * 1024).spawn(move || {
        let outcome = match Upload::new(&ctx, &conversation_id, &client_id, &files) {
            Ok(mut upload) => {
                let outcome = work(&mut upload);
                match outcome {
                    Ok(message) => {
                        upload.done();
                        Ok(message)
                    }
                    Err(_) if upload.cancelled() => Err(CANCELLED.to_owned()),
                    Err(why) => Err(why),
                }
            }
            Err(why) => Err(why),
        };
        ctx.sent(&conversation_id, &client_id, outcome);
    });
    if let Err(err) = spawned {
        answer.sent(&conversation, &client, Err(format!("The files could not be sent: {err}.")));
    }
}

type Cancels = Vec<(AccountKey, String, Arc<AtomicBool>, bool)>;

/// Uploads under way and cancels that came first, as `(account, client id, flag, under way)`.
fn cancels() -> &'static Mutex<Cancels> {
    static CANCELS: std::sync::OnceLock<Mutex<Cancels>> = std::sync::OnceLock::new();
    CANCELS.get_or_init(Mutex::default)
}

/// The user cancelled a send. Its upload stops at the next buffer or part; a send whose upload
/// has not begun is cancelled when it does.
pub fn cancel(account: &AccountKey, client_id: &str) {
    let mut held = crate::hub::lock(cancels());
    if let Some((_, _, flag, _)) = held.iter().find(|(of, id, _, _)| of == account && id == client_id) {
        flag.store(true, Ordering::Relaxed);
        return;
    }
    let early = held.iter().filter(|(_, _, _, under_way)| !under_way).count();
    if early >= EARLY_CANCELS {
        if let Some(oldest) = held.iter().position(|(_, _, _, under_way)| !under_way) {
            held.remove(oldest);
        }
    }
    held.push((account.clone(), client_id.to_owned(), Arc::new(AtomicBool::new(true)), false));
}

fn register(account: &AccountKey, client_id: &str) -> Arc<AtomicBool> {
    let mut held = crate::hub::lock(cancels());
    if let Some(entry) = held.iter_mut().find(|(of, id, _, _)| of == account && id == client_id) {
        entry.3 = true;
        return entry.2.clone();
    }
    let flag = Arc::new(AtomicBool::new(false));
    held.push((account.clone(), client_id.to_owned(), flag.clone(), true));
    flag
}

fn unregister(account: &AccountKey, client_id: &str) {
    crate::hub::lock(cancels()).retain(|(of, id, _, _)| !(of == account && id == client_id));
}

/// A pending message was dropped (`Request::Discard`), or the demo sent one: its `outbox/`
/// copies go. Files elsewhere are the user's.
pub fn discard(files: &[OutgoingFile]) {
    for file in files {
        if let Ok((path, true)) = allowed(&file.path) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Where the shell stages pasted pixels.
pub fn outbox() -> PathBuf {
    protocol::support_dir().join("outbox")
}

/// Whether a path may be read for sending: its real place, and whether it is in `outbox/`. The
/// error finishes a sentence that starts with the file's name.
pub fn allowed(path: &str) -> Result<(PathBuf, bool), String> {
    let refused = || "is not a file tinyclient can send.".to_owned();
    let given = Path::new(path);
    if !given.is_absolute() {
        return Err(refused());
    }
    let link = std::fs::symlink_metadata(given).map_err(|_| "is no longer there.".to_owned())?;
    let real = given.canonicalize().map_err(|_| "is no longer there.".to_owned())?;
    let outbox = outbox();
    let outbox_real = outbox.canonicalize().ok();
    let in_outbox = outbox_real.as_ref().is_some_and(|dir| real.starts_with(dir));
    let named_in_outbox = given.starts_with(&outbox) || outbox_real.as_ref().is_some_and(|dir| given.starts_with(dir));
    // What the shell staged is a plain file it wrote. A link there could point anywhere.
    if named_in_outbox && (link.file_type().is_symlink() || !in_outbox) {
        return Err(refused());
    }
    if !in_outbox && forbidden(&real) {
        return Err(refused());
    }
    if real.is_dir() {
        return Err("is a folder. Folders can't be sent. Zip it first.".to_owned());
    }
    if !real.is_file() {
        return Err(refused());
    }
    Ok((real, in_outbox))
}

/// Places a file to send never comes from, by their real path.
fn forbidden(real: &Path) -> bool {
    #[cfg(windows)]
    if windows_places::Places::here().refuse(&real.to_string_lossy()) {
        return true;
    }
    let support = protocol::support_dir();
    let own = support.canonicalize().unwrap_or(support);
    if real.starts_with(&own) {
        return true;
    }
    if ["/dev", "/proc", "/sys", "/etc", "/private/etc", "/var/root", "/private/var/root"].iter().any(|dir| real.starts_with(dir)) {
        return true;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute()) else {
        return false;
    };
    let home = home.canonicalize().unwrap_or(home);
    let Ok(inside) = real.strip_prefix(&home) else {
        return false;
    };
    let mut parts = inside.components();
    let first = parts.next().map(|part| part.as_os_str().to_string_lossy().into_owned());
    // A hidden directory (`~/.ssh`, `~/.config`), not a hidden file the user picked.
    let hidden_dir = first.as_deref().is_some_and(|first| first.starts_with('.')) && parts.next().is_some();
    hidden_dir || first.as_deref() == Some("Library")
}

/// Windows's places a file to send never comes from, mirroring `forbidden`'s Unix ones: tinyclient's
/// own folder, the system's secrets (`System32\config` for `/etc`), the profile's hidden folders
/// (`%USERPROFILE%\.ssh`), and AppData for `~/Library`, whose credentials, DPAPI keys and browser
/// profiles are named again in case AppData is moved. Paths compare case-insensitively, as
/// Windows does.
#[cfg(any(windows, test))]
mod windows_places {
    /// A path folded for comparing: no `\\?\`, `\` between parts, lower case, no `\` at the end.
    pub fn fold(path: &str) -> String {
        let path = match path.strip_prefix(r"\\?\UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => path.strip_prefix(r"\\?\").unwrap_or(path).to_owned(),
        };
        path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }

    /// Whether folded `path` is `dir` or inside it; `C:\a\bc` is not inside `C:\a\b`.
    fn under(path: &str, dir: &str) -> bool {
        !dir.is_empty() && path.strip_prefix(dir).is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
    }

    /// The places, each folded once.
    pub struct Places {
        /// Never, whatever else holds: tinyclient's own, the system's and the browsers' secrets.
        never: Vec<String>,
        /// AppData, the way `~/Library` is: never, except below `temp`.
        app_data: Vec<String>,
        /// The temporary folder, inside AppData\Local: what Explorer unpacks from a zip to send
        /// lands there.
        temp: Vec<String>,
        /// `%USERPROFILE%`, for its hidden folders.
        profile: Option<String>,
    }

    impl Places {
        pub fn new(never: &[String], app_data: &[String], temp: &[String], profile: Option<&str>) -> Places {
            let folded = |dirs: &[String]| dirs.iter().map(|dir| fold(dir)).filter(|dir| !dir.is_empty()).collect();
            Places {
                never: folded(never),
                app_data: folded(app_data),
                temp: folded(temp),
                profile: profile.map(fold).filter(|dir| !dir.is_empty()),
            }
        }

        /// This user's, from the environment, by real path where the folder is there.
        #[cfg(windows)]
        pub fn here() -> Places {
            use std::path::PathBuf;
            let real = |path: PathBuf| path.canonicalize().unwrap_or(path).to_string_lossy().into_owned();
            let var = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|dir| dir.is_absolute());
            let profile = var("USERPROFILE");
            let roaming = var("APPDATA").or_else(|| profile.as_ref().map(|dir| dir.join(r"AppData\Roaming")));
            let local = var("LOCALAPPDATA").or_else(|| profile.as_ref().map(|dir| dir.join(r"AppData\Local")));
            let system = var("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let mut never = vec![real(protocol::support_dir()), real(system.join(r"System32\config"))];
            let within = |base: &Option<PathBuf>, parts: &[&str]| -> Vec<String> {
                base.iter().flat_map(|base| parts.iter().map(move |part| real(base.join(part)))).collect()
            };
            never.extend(within(&roaming, &[r"Microsoft\Credentials", r"Microsoft\Protect", r"Microsoft\Crypto", r"Mozilla\Firefox"]));
            never.extend(within(&local, &[r"Microsoft\Credentials", r"Google\Chrome\User Data", r"Microsoft\Edge\User Data", r"Mozilla\Firefox"]));
            let app_data: Vec<String> = [profile.as_ref().map(|dir| dir.join("AppData")), roaming, local].into_iter().flatten().map(real).collect();
            let temp: Vec<String> = [var("TEMP"), var("TMP"), Some(std::env::temp_dir())].into_iter().flatten().map(real).collect();
            Places::new(&never, &app_data, &temp, profile.map(real).as_deref())
        }

        /// Whether the file at real path `real` must not be sent.
        pub fn refuse(&self, real: &str) -> bool {
            let real = fold(real);
            // The device namespace, as `/dev` is.
            if real.starts_with(r"\\.\") {
                return true;
            }
            if self.never.iter().any(|dir| under(&real, dir)) {
                return true;
            }
            if self.app_data.iter().any(|dir| under(&real, dir)) && !self.temp.iter().any(|dir| under(&real, dir)) {
                return true;
            }
            let Some(inside) = self.profile.as_deref().and_then(|profile| real.strip_prefix(profile)).and_then(|rest| rest.strip_prefix('\\')) else {
                return false;
            };
            // A hidden folder (`.ssh`, `.aws`), not a hidden file the user picked.
            let mut parts = inside.split('\\').filter(|part| !part.is_empty());
            parts.next().is_some_and(|first| first.starts_with('.')) && parts.next().is_some()
        }
    }
}

/// Whether the network takes uploads at this address. Never plain HTTP, except a test's own
/// server in a scratch directory.
fn uploads_to(network: Network, url: &str) -> bool {
    let Some((secure, host)) = super::fetch::host_of(url) else {
        return false;
    };
    if !crate::session::enabled() {
        let local = std::env::var("TINYCLIENT_MEDIA_HOSTS").unwrap_or_default();
        if host == "127.0.0.1" || host == "localhost" || local.split(',').any(|named| named == host) {
            return true;
        }
    }
    if !secure {
        return false;
    }
    let hosts: &[&str] = match network {
        // The API, and the bucket its attachment URLs point at.
        Network::Discord => &["discord.com", "discordapp.com", "discord-attachments-uploads-prd.storage.googleapis.com"],
        // `files.getUploadURLExternal` answers an address on files.slack.com.
        Network::Slack => &["slack.com"],
        // OneDrive and SharePoint for files; the Teams media service for pasted images.
        Network::Teams => &["graph.microsoft.com", "sharepoint.com", "asm.skype.com", "teams.microsoft.com"],
        // MTProto, through `parts`: nothing goes over HTTP.
        Network::Telegram => &[],
        // Encrypted before it leaves, by the adapter: nothing goes through this helper.
        Network::Signal => &[],
    };
    hosts.iter().any(|name| super::fetch::under(&host, name))
}

fn agent() -> ureq::Agent {
    use ureq::tls::{TlsConfig, TlsProvider};
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
        .timeout_connect(Some(Duration::from_secs(10)))
        // A large file on a slow line is one request, so nothing limits the body; the answer
        // after it may take the network a while to make.
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .user_agent(crate::adapters::net::browser())
        .build()
        .into()
}

/// The name the network shows: one plain file name, never a path.
fn shown(file: &OutgoingFile) -> String {
    let named = match file.name.trim() {
        "" => Path::new(&file.path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        name => name.to_owned(),
    };
    let mut name: String = named.chars().map(|c| if c.is_control() || c == '/' || c == '\\' { '_' } else { c }).collect();
    if name.is_empty() || name == "." || name == ".." {
        name = "file".to_owned();
    }
    protocol::clip(&mut name, 200);
    name
}

/// A name inside a quoted header parameter.
fn quoted(text: &str) -> String {
    text.chars().map(|c| if c == '"' || c == '\\' || c.is_control() { '_' } else { c }).collect()
}

fn with_default(target: &Target, header: &str, value: &str) -> Target {
    let named = target.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case(header));
    match named {
        true => target.clone(),
        false => target.clone().header(header, value),
    }
}

fn plain(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "it is no longer there".to_owned(),
        std::io::ErrorKind::PermissionDenied => "tinyclient may not read it".to_owned(),
        _ => err.to_string(),
    }
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3))
}

/// The type a network is told, from the name's extension.
pub fn mime_of(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_plain_file_name_and_types_come_from_the_extension() {
        let staged = |path: &str, name: &str| OutgoingFile {
            path: path.to_owned(),
            name: name.to_owned(),
            spoiler: false,
            as_file: false,
            original: false,
        };
        assert_eq!(shown(&staged("/a/b/cat.PNG", "")), "cat.PNG");
        assert_eq!(shown(&staged("/a/b/cat.png", "../../x\ny")), ".._.._x_y");
        assert_eq!(shown(&staged("/a/b/..", "")), "file");
        assert_eq!(mime_of("cat.PNG"), "image/png");
        assert_eq!(mime_of("notes"), "application/octet-stream");
        assert_eq!(quoted("a\"b\r\n"), "a_b__");
    }

    #[test]
    fn uploads_go_only_to_the_networks_own_hosts_over_https() {
        assert!(uploads_to(Network::Discord, "https://discord-attachments-uploads-prd.storage.googleapis.com/x?upload_id=1"));
        assert!(uploads_to(Network::Slack, "https://files.slack.com/upload/v1/abc"));
        assert!(uploads_to(Network::Teams, "https://contoso-my.sharepoint.com/personal/x/_api/upload"));
        assert!(!uploads_to(Network::Discord, "https://storage.googleapis.com/other-bucket/x"));
        assert!(!uploads_to(Network::Slack, "http://files.slack.com/upload/v1/abc"));
        assert!(!uploads_to(Network::Slack, "https://files.slack.com.example.org/upload"));
        assert!(!uploads_to(Network::Telegram, "https://core.telegram.org/x"));
    }

    #[test]
    fn windows_places_are_matched_by_real_path_whatever_the_case() {
        let owned = |dirs: &[&str]| dirs.iter().map(|dir| dir.to_string()).collect::<Vec<_>>();
        let places = windows_places::Places::new(
            &owned(&[r"C:\Users\Me\AppData\Roaming\tinyclient", r"C:\Windows\System32\config", r"D:\Moved\Microsoft\Credentials"]),
            &owned(&[r"\\?\C:\Users\Me\AppData\", r"D:\Moved"]),
            &owned(&[r"C:\Users\Me\AppData\Local\Temp"]),
            Some(r"C:\Users\Me"),
        );
        for refused in [
            r"\\?\C:\Users\me\appdata\roaming\TINYCLIENT\chat.sqlite",
            r"C:\WINDOWS\system32\CONFIG\SAM",
            r"C:\Users\Me\.ssh\id_ed25519",
            r"c:/users/me/.aws/credentials",
            r"C:\Users\Me\AppData\Local\Google\Chrome\User Data\Default\Cookies",
            r"C:\Users\Me\AppData\Roaming\Microsoft\Protect\S-1-5\key",
            r"D:\moved\microsoft\credentials\blob",
            r"D:\Moved\Mozilla\Firefox\Profiles\x\logins.json",
            r"\\.\PhysicalDrive0",
        ] {
            assert!(places.refuse(refused), "{refused} is refused");
        }
        for sent in [
            r"\\?\C:\Users\Me\Documents\notes.txt",
            r"C:\Users\Me\.bashrc",
            r"C:\Users\Me\AppDataX\notes.txt",
            r"C:\Users\Me\AppData\Local\Temp\Temp1_photos.zip\cat.png",
            r"C:\Windows\System32\configuration.txt",
            r"\\?\UNC\server\share\report.pdf",
        ] {
            assert!(!places.refuse(sent), "{sent} may be sent");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_secrets_are_refused_and_a_picked_file_is_not() {
        let var = |name: &str| PathBuf::from(std::env::var_os(name).unwrap());
        let real = |path: PathBuf| path.canonicalize().unwrap_or(path);
        assert!(forbidden(&real(var("SystemRoot").join(r"System32\config\SAM"))));
        assert!(forbidden(&real(var("USERPROFILE")).join(r".ssh\id_ed25519")));
        assert!(forbidden(&real(var("APPDATA")).join(r"Microsoft\Credentials\blob")));
        assert!(forbidden(&real(var("LOCALAPPDATA")).join(r"Microsoft\Edge\User Data\Default\Login Data")));
        let upper = real(var("APPDATA")).to_string_lossy().to_uppercase();
        assert!(forbidden(Path::new(&format!(r"{upper}\Microsoft\Protect\key"))), "{upper} in capitals");
        let picked = std::env::temp_dir().join(format!("tinyclient-picked-{}.txt", std::process::id()));
        std::fs::write(&picked, b"hello").unwrap();
        let answer = allowed(&picked.to_string_lossy());
        std::fs::remove_file(&picked).unwrap();
        assert!(answer.is_ok(), "{answer:?}");
    }
}
