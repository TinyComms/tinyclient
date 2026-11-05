//! Adding a new device of the person from one they already have, with their history, through the
//! server's link relay (`e2ee_device_link`; tinyserver `docs/e2ee-text.md` section 5, and the client
//! contract in tiny's `research_notes/device-link-protocol.md`). The `e2ee` crate's `link` and
//! `archive` do the cryptography; this is the part with a network, a disk and a person.
//!
//! The NEW device makes a link and shows its code. The person gives the code to a device they
//! have (the OLD one), which answers with its key; both show six digits. The new device says who it
//! is; the old one shows that and asks the person to type the digits the new one shows. Then it
//! cross-signs the new device, packs what its cache opened (all of it, the last N days, or none)
//! into an archive under a fresh key, uploads it, and sends the key and the signature over the
//! channel. The new device registers with the signature, downloads, checks and merges the
//! archive, and says done. No key of either device, and nothing in the clear, goes to the server.
//!
//! One link at a time per account, on a thread of its own: it waits on the relay's long poll, and
//! takes this device's lock only for a moment at a time. Each step is told to the shell
//! (`Change::Tiny`), which reads `E2eeAsk::Link`.

use super::{
    b64, device_label, fingerprint, lock, private_dir, unb64, Device, Opened, Saved, Shared,
};
use e2ee::archive::{self, ArchiveKey, Meta, Target, Writer};
use e2ee::link::{self, ArchiveRef, Channel, LinkCode, Message, Pending, Role, Scope};
use e2ee::store::CacheKey;
use e2ee::DeviceId;
use protocol::{E2eeHistory, E2eeLink, LinkStage};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Called when the link moved on, so the shell reads it again.
pub(crate) type Told = Arc<dyn Fn() + Send + Sync>;

/// How long the link waits for the other device or the person: the relay keeps a link 5 minutes.
const PATIENCE: Duration = Duration::from_secs(5 * 60);
/// One long poll of the relay. Short enough that a cancel is noticed soon.
const WAIT: u64 = 10;
/// The most history one link carries when the server does not say (`e2ee_link_archive_max`).
pub(super) const ARCHIVE_MAX: u64 = 96 * 1024 * 1024;
/// The archive on its way, in this account's private folder.
const ARCHIVE_FILE: &str = "link-archive.part";
/// The choices of history the old device offers, as `Scope`s.
const CHOICES: [Scope; 4] = [Scope::Days(7), Scope::Days(30), Scope::All, Scope::None];

/// What the relay and the device routes are to a link: the HTTP one, or a fake in the tests.
pub(super) trait Relay: Send + Sync {
    /// A new link: its id (base64url) and when it runs out.
    fn create(&self) -> Result<(String, String), Failed>;
    fn post(&self, link: &str, from: Role, data: &[u8]) -> Result<(), Failed>;
    /// The messages after `after`, waiting up to `wait` seconds for one: `(seq, from, data)`.
    fn read(
        &self,
        link: &str,
        after: i64,
        wait: u64,
    ) -> Result<Vec<(i64, String, Vec<u8>)>, Failed>;
    fn put_archive(&self, link: &str, path: &Path, bytes: u64) -> Result<(), Failed>;
    /// The archive into `to`, at most `max` bytes. Answers how many.
    fn get_archive(
        &self,
        link: &str,
        to: &Path,
        max: u64,
        moved: &dyn Fn(u64),
    ) -> Result<u64, Failed>;
    fn delete(&self, link: &str);
    /// `PUT /v1/e2ee/devices/{id}` for this new device, with the old one's cross-signature.
    fn register(
        &self,
        device_id: &str,
        signature_key: &[u8],
        cross_signature: &[u8],
    ) -> Result<(), Failed>;
    /// Publish only public MLS packages before acknowledging a link, so the approving
    /// device's immediate group sync can actually add this device.
    fn publish_keys(
        &self,
        device_id: &str,
        packages: &[Vec<u8>],
        last_resort: &[u8],
    ) -> Result<(), Failed>;
    /// The conversations the person is in: an archive's rows for any other are dropped.
    fn conversations(&self) -> Result<BTreeSet<String>, Failed>;
}

#[derive(Debug)]
pub(super) enum Failed {
    /// The link is gone: it ran out, or the other device deleted it.
    Gone,
    Words(String),
}

impl From<Failed> for String {
    fn from(failed: Failed) -> String {
        match failed {
            Failed::Gone => {
                "The link ran out or was stopped on the other device. Start again.".to_owned()
            }
            Failed::Words(words) => words,
        }
    }
}

fn words(error: e2ee::Error) -> String {
    match error {
        e2ee::Error::Malformed("link code") => {
            "That isn't a link code. Copy it again from the new device.".to_owned()
        }
        e2ee::Error::Authentication => {
            "The other device's answer couldn't be checked, so the link was stopped. Start again."
                .to_owned()
        }
        _ => "The link couldn't be made safe, so it was stopped. Start again.".to_owned(),
    }
}

/// One link of one account.
pub(super) struct Linking {
    state: Mutex<State>,
    wake: Condvar,
    told: Told,
    approval: Arc<dyn protocol::device_approval::DeviceApproval>,
    deadline: Instant,
}

#[derive(Default)]
struct State {
    view: E2eeLink,
    stop: bool,
    /// The person's yes on the old device: the history to send.
    confirmed: Option<Scope>,
    /// At the time of `Done` or `Failed`: the view stays until another link starts.
    over: bool,
}

impl Linking {
    fn new(role: Role, told: Told) -> Linking {
        Linking {
            state: Mutex::new(State {
                view: E2eeLink {
                    role: role.as_str().to_owned(),
                    ..E2eeLink::default()
                },
                ..State::default()
            }),
            wake: Condvar::new(),
            told,
            approval: super::approval::service(),
            deadline: Instant::now() + PATIENCE,
        }
    }

    /// Changes the view and tells the shell.
    fn show(&self, change: impl FnOnce(&mut E2eeLink)) {
        change(&mut lock(&self.state).view);
        (self.told)();
    }

    fn stopped(&self) -> bool {
        lock(&self.state).stop
    }

    fn end(&self, result: Result<u64, String>) {
        {
            let mut state = lock(&self.state);
            state.over = true;
            match result {
                Ok(messages) => {
                    state.view.stage = LinkStage::Done;
                    state.view.messages = messages;
                    state.view.code.clear();
                }
                Err(why) => {
                    state.view.stage = LinkStage::Failed;
                    state.view.error = why;
                    state.view.code.clear();
                }
            }
        }
        self.wake.notify_all();
        (self.told)();
    }

    /// Waits for the person to confirm on the old device, or to stop.
    fn confirmed(&self) -> Result<Scope, String> {
        let deadline = Instant::now() + PATIENCE;
        let mut state = lock(&self.state);
        loop {
            if state.stop {
                return Err(STOPPED.to_owned());
            }
            if let Some(scope) = state.confirmed.take() {
                return Ok(scope);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(
                    "Nobody confirmed in time, so the link was stopped. Start again.".to_owned(),
                );
            }
            state = self
                .wake
                .wait_timeout(state, left)
                .map(|(state, _)| state)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
    }
}

const STOPPED: &str = "The link was stopped.";

type Links = Mutex<HashMap<(String, String), Arc<Linking>>>;

fn links() -> &'static Links {
    static LINKS: OnceLock<Links> = OnceLock::new();
    LINKS.get_or_init(Mutex::default)
}

fn key_of(saved: &Saved) -> (String, String) {
    (saved.base_url.clone(), saved.user_id.clone())
}

/// The link in progress (or just over) on this device.
pub(crate) fn view(saved: &Saved) -> Option<E2eeLink> {
    let linking = lock(links()).get(&key_of(saved)).cloned()?;
    let view = lock(&linking.state).view.clone();
    Some(view)
}

/// A new link, unless one is still going. The last one's view, if it is over, is replaced.
fn begin(saved: &Saved, role: Role, told: Told) -> Result<Arc<Linking>, String> {
    let mut all = lock(links());
    if let Some(going) = all.get(&key_of(saved)) {
        if !lock(&going.state).over {
            return Err("A link is already going on this device. Stop it first.".to_owned());
        }
    }
    let linking = Arc::new(Linking::new(role, told));
    all.insert(key_of(saved), linking.clone());
    Ok(linking)
}

/// The person typed the six digits the new device shows, and chose how much history to send.
pub(crate) fn confirm(saved: &Saved, history: &str, check: &str) -> Result<(), String> {
    let linking = lock(links())
        .get(&key_of(saved))
        .cloned()
        .ok_or("No link is going on this device.")?;
    let scope = Scope::parse(history).map_err(|_| super::super::CHECK.to_owned())?;
    let typed: String = check.chars().filter(char::is_ascii_digit).collect();
    {
        let mut state = lock(&linking.state);
        if state.view.role != "old"
            || state.view.stage != LinkStage::Confirm
            || state.stop
            || state.over
        {
            return Err("This link isn't waiting for you to confirm.".to_owned());
        }
        if !super::subtle_eq(typed.as_bytes(), state.view.check.as_bytes()) {
            // A wrong number is a code that went to the wrong device, or someone in between.
            state.stop = true;
            drop(state);
            linking.wake.notify_all();
            return Err("Those digits don't match the new device, so the link was stopped. If you typed them wrong, start again.".to_owned());
        }
        state.confirmed = Some(scope);
    }
    linking.wake.notify_all();
    Ok(())
}

/// When to ask about the groups after the person's devices changed: past the server's commit
/// batching (`e2ee_commit_batch_secs`, 30 by default), and once more in case one was busy.
const NUDGES: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(35),
    Duration::from_secs(40),
];

/// Settle a device addition or removal immediately, so a linked device gets its Welcome before
/// its first send. Older servers batch membership changes, so retain the later retries too.
pub(super) fn nudge(device: Shared) {
    let _ = std::thread::Builder::new()
        .name("tiny-e2ee-nudge".into())
        .spawn(move || {
            for wait in NUDGES {
                std::thread::sleep(wait);
                let conversations = lock(&device).known_encrypted();
                for conversation in conversations {
                    // One at a time: reading and sending go on in between.
                    let mut device = lock(&device);
                    if !device.ready() {
                        return;
                    }
                    if let Err(why) = device.sync(&conversation) {
                        tracing::debug!("chatd: Tiny e2ee: {conversation}: {why}");
                    }
                }
            }
        });
}

pub(crate) fn cancel(saved: &Saved) {
    if let Some(linking) = lock(links()).get(&key_of(saved)).cloned() {
        lock(&linking.state).stop = true;
        linking.wake.notify_all();
    }
}

/// On a device waiting to be approved: makes a link and runs the new device's half.
pub(super) fn start_new(
    saved: &Saved,
    device: &Shared,
    linking: bool,
    told: Told,
) -> Result<(), String> {
    if !linking {
        return Err("This server can't link devices yet. Copy the link code below to a device you already have instead.".to_owned());
    }
    {
        let device = lock(device);
        if device.engine.is_none() {
            return Err("Encryption isn't set up on this device.".to_owned());
        }
        if device.ready() {
            return Err("This device is already set up.".to_owned());
        }
    }
    let job = begin(saved, Role::New, told)?;
    let (saved, device) = (saved.clone(), device.clone());
    std::thread::Builder::new()
        .name("tiny-e2ee-link".into())
        .spawn(move || {
            let relay = Http { saved };
            let result = run_new(&job, &device, &relay);
            // Registered by now unless it failed early; either way the device reads where it
            // stands.
            lock(&device).enrol();
            job.end(result);
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// On a device the person has: takes the new device's code and runs the old device's half.
pub(super) fn start_old(
    saved: &Saved,
    device: &Shared,
    code: &str,
    told: Told,
) -> Result<(), String> {
    if !lock(device).ready() {
        return Err("This device has to be set up before it can approve another.".to_owned());
    }
    let code = LinkCode::parse(code).map_err(words)?;
    let job = begin(saved, Role::Old, told)?;
    let (saved, device) = (saved.clone(), device.clone());
    std::thread::Builder::new()
        .name("tiny-e2ee-link".into())
        .spawn(move || {
            let relay = Http { saved };
            let result = run_old(&job, &device, &relay, &code).map(|()| 0);
            if result.is_ok() {
                // In every group soon, so it can send at once, not when someone else next does.
                nudge(device.clone());
            }
            job.end(result);
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// The first message from `from` after `after`, moving `after` on. Gives up when the link is
/// stopped here or runs out.
fn next_from(
    job: &Linking,
    relay: &dyn Relay,
    link: &str,
    from: Role,
    after: &mut i64,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if job.stopped() {
            return Err(STOPPED.to_owned());
        }
        if Instant::now() > deadline {
            return Err("The other device didn't answer in time. Start again.".to_owned());
        }
        for (seq, sender, data) in relay.read(link, *after, WAIT)? {
            *after = (*after).max(seq);
            if sender == from.as_str() {
                return Ok(data);
            }
        }
    }
}

/// Opens the other device's next message. An `abort` ends the link with its reason.
fn receive(channel: &mut Channel, sealed: &[u8]) -> Result<Message, String> {
    match channel.receive(sealed).map_err(words)? {
        Message::Abort { reason } => Err(if reason.is_empty() {
            "The other device stopped the link.".to_owned()
        } else {
            format!("The other device stopped the link: {reason}")
        }),
        message => Ok(message),
    }
}

/// Tells the other device the link is off, best effort, and deletes it.
fn abandon(relay: &dyn Relay, link: &str, channel: Option<&mut Channel>, from: Role, why: &str) {
    if let Some(channel) = channel {
        let reason = if why == STOPPED { "stopped" } else { "failed" };
        if let Ok(sealed) = channel.send(&Message::Abort {
            reason: reason.to_owned(),
        }) {
            let _ = relay.post(link, from, &sealed);
        }
    }
    relay.delete(link);
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        "android" => "Android",
        "ios" => "iOS",
        _ => "",
    }
}

/// The new device's half. Answers how many messages it took in.
pub(super) fn run_new(job: &Linking, device: &Shared, relay: &dyn Relay) -> Result<u64, String> {
    let (link_text, expires_at) = relay.create()?;
    let link_id =
        link::parse_link_id(&link_text).map_err(|_| super::super::UNREADABLE.to_owned())?;
    let pending = Pending::create(link_id).map_err(words)?;
    job.show(|view| {
        view.stage = LinkStage::Waiting;
        view.code = pending.code().encode().to_string();
        view.expires_at = expires_at;
    });
    let mut channel = None;
    let mut after = 0;
    let result = new_half(
        job,
        device,
        relay,
        &link_text,
        &link_id,
        &pending,
        &mut channel,
        &mut after,
    );
    match &result {
        Ok(_) => {
            // The old device deletes the link once it has read `done`; deleting it first would
            // take `done` with it. Wait for that, a little, then delete it anyway.
            for _ in 0..3 {
                match relay.read(&link_text, after, WAIT) {
                    // This device's own `done` comes back too: past it, the read waits.
                    Ok(rows) => after = rows.iter().fold(after, |at, (seq, _, _)| at.max(*seq)),
                    Err(_) => break,
                }
            }
            relay.delete(&link_text);
        }
        Err(why) => abandon(relay, &link_text, channel.as_mut(), Role::New, why),
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn new_half(
    job: &Linking,
    device: &Shared,
    relay: &dyn Relay,
    link: &str,
    link_id: &[u8; 16],
    pending: &Pending,
    channel_out: &mut Option<Channel>,
    after: &mut i64,
) -> Result<u64, String> {
    let old_pub = next_from(job, relay, link, Role::Old, after)?;
    let channel = channel_out.insert(pending.accept(&old_pub).map_err(words)?);
    let (device_id, signature_key, user, dir, most) = {
        let device = lock(device);
        let engine = device
            .engine
            .as_ref()
            .ok_or("Encryption isn't set up on this device.")?;
        (
            engine.device_id(),
            engine.signature_key(),
            device.saved.user_id.clone(),
            device.dir.clone(),
            device.link_max,
        )
    };
    let hello = Message::Hello {
        device_id: device_id.to_string(),
        signature_key: b64(&signature_key),
        label: device_label(),
        os: os_name().to_owned(),
    };
    relay.post(link, Role::New, &channel.send(&hello).map_err(words)?)?;
    let check = channel.check_digits().to_owned();
    job.show(|view| {
        view.stage = LinkStage::Confirm;
        view.check = check;
        view.code.clear();
    });

    let sealed = next_from(job, relay, link, Role::Old, after)?;
    let Message::Approve {
        cross_signature,
        archive,
        ..
    } = receive(channel, &sealed)?
    else {
        return Err(
            "The other device said something out of turn, so the link was stopped.".to_owned(),
        );
    };
    job.show(|view| {
        view.stage = LinkStage::Receiving;
        view.total = archive.as_ref().map_or(0, |archive| archive.bytes);
    });
    let cross_signature = unb64(&cross_signature).ok_or(super::super::UNREADABLE)?;
    let mut messages = 0;
    if let Some(archive) = archive {
        messages = take_in(
            job, device, relay, link, link_id, &user, &dir, &archive, most,
        )?;
    }
    if job.stopped() {
        return Err(STOPPED.to_owned());
    }
    relay.register(&device_id.to_string(), &signature_key, &cross_signature)?;
    let (packages, last_resort) = {
        let mut device = lock(device);
        let engine = device.engine()?;
        (
            engine.key_packages(super::KEY_PACKAGES).map_err(words)?,
            engine.last_resort_key_package().map_err(words)?,
        )
    };
    relay.publish_keys(&device_id.to_string(), &packages, &last_resort)?;
    relay.post(
        link,
        Role::New,
        &channel.send(&Message::Done {}).map_err(words)?,
    )?;
    Ok(messages)
}

/// Downloads the archive, checks it is what the old device said it uploaded, and merges it.
#[allow(clippy::too_many_arguments)]
fn take_in(
    job: &Linking,
    device: &Shared,
    relay: &dyn Relay,
    link: &str,
    link_id: &[u8; 16],
    user: &str,
    dir: &Path,
    archive: &ArchiveRef,
    most: u64,
) -> Result<u64, String> {
    let unreadable = "The history that came couldn't be checked, so none of it was taken in.";
    let key: [u8; 32] = unb64(&archive.key)
        .and_then(|key| key.try_into().ok())
        .ok_or(unreadable)?;
    let key = ArchiveKey::from_bytes(key);
    let sha256: [u8; 32] = unb64(&archive.sha256)
        .and_then(|sum| sum.try_into().ok())
        .ok_or(unreadable)?;
    if archive.bytes > most {
        return Err("That history is more than this device takes at once.".to_owned());
    }
    private_dir(dir).map_err(|err| format!("The encryption folder could not be made: {err}"))?;
    let path = dir.join(ARCHIVE_FILE);
    let result = (|| {
        let got = relay.get_archive(link, &path, archive.bytes, &|done| {
            lock(&job.state).view.done = done;
        })?;
        let mut file = std::fs::File::open(&path).map_err(|err| err.to_string())?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher).map_err(|err| err.to_string())?;
        if got != archive.bytes || !super::subtle_eq(&hasher.finalize(), &sha256) {
            return Err(unreadable.to_owned());
        }
        let members = relay.conversations()?;
        let mut device = lock(device);
        let mut into = Merging {
            device: &mut device,
            members,
            from: String::new(),
            touched: BTreeSet::new(),
        };
        let open = || std::fs::File::open(&path).map_err(|err| e2ee::Error::Io(err.to_string()));
        let (meta, merged) = archive::merge(&key, link_id, user, open, &mut into)
            .map_err(|_| unreadable.to_owned())?;
        tracing::info!(
            "chatd: Tiny e2ee: a device link took in {} messages ({} kept, {} refused) from {}",
            merged.added,
            merged.kept,
            merged.refused,
            meta.from_device
        );
        into.device.save_notes();
        into.device.reload(&into.touched);
        Ok(merged.added)
    })();
    let _ = std::fs::remove_file(&path);
    result
}

/// A device's cache, as an archive (a link's, or a backup file's) merges into it.
pub(super) struct Merging<'a> {
    pub device: &'a mut Device,
    pub members: BTreeSet<String>,
    /// The device the archive is from, from its first record.
    pub from: String,
    /// Conversations that gained rows: read again once merged.
    pub touched: BTreeSet<String>,
}

impl Target for Merging<'_> {
    fn validate(&self, conv: &str, id: &str, opened: &Value) -> e2ee::Result<()> {
        if !super::super::safe_id(conv) || !super::super::safe_id(id) {
            return Err(e2ee::Error::Malformed("archive message id"));
        }
        serde_json::from_value::<Opened>(opened.clone())
            .map(|_| ())
            .map_err(|_| e2ee::Error::Malformed("archive message"))
    }

    fn start(&mut self, meta: &Meta) {
        self.from = meta.from_device.chars().take(64).collect();
    }

    fn is_member(&mut self, conv: &str) -> bool {
        super::super::safe_id(conv) && self.members.contains(conv)
    }

    fn has(&mut self, conv: &str, id: &str) -> bool {
        self.device
            .cache(conv)
            .is_some_and(|cache| cache.rows.contains_key(id))
    }

    fn put(&mut self, conv: &str, id: &str, opened: Value) -> e2ee::Result<()> {
        let mut opened: Opened = serde_json::from_value(opened)
            .map_err(|_| e2ee::Error::Malformed("archive message"))?;
        if !super::super::safe_id(id) {
            return Ok(());
        }
        opened.imported_from = Some(self.from.clone());
        self.device.remember(conv, id, opened);
        self.touched.insert(conv.to_owned());
        Ok(())
    }

    fn verified(&mut self, user: &str, number: &str) -> e2ee::Result<()> {
        if super::super::safe_id(user) {
            self.device
                .notes
                .verified
                .entry(user.to_owned())
                .or_insert_with(|| number.to_owned());
        }
        Ok(())
    }
}

/// The old device's half.
pub(super) fn run_old(
    job: &Linking,
    device: &Shared,
    relay: &dyn Relay,
    code: &LinkCode,
) -> Result<(), String> {
    let link_text = code.link_id_text();
    let mut channel = None;
    let result = old_half(job, device, relay, code, &link_text, &mut channel);
    match &result {
        Ok(()) => relay.delete(&link_text),
        Err(why) => abandon(relay, &link_text, channel.as_mut(), Role::Old, why),
    }
    result
}

fn old_half(
    job: &Linking,
    device: &Shared,
    relay: &dyn Relay,
    code: &LinkCode,
    link: &str,
    channel_out: &mut Option<Channel>,
) -> Result<(), String> {
    let (old_pub, channel) = link::join(code).map_err(words)?;
    let channel = channel_out.insert(channel);
    relay
        .post(link, Role::Old, &old_pub)
        .map_err(|failed| match failed {
            Failed::Gone => {
                "That code ran out or was already used. Show a new one on the new device."
                    .to_owned()
            }
            Failed::Words(words) => words,
        })?;
    let mut after = 0;
    let sealed = next_from(job, relay, link, Role::New, &mut after)?;
    let Message::Hello {
        device_id,
        signature_key,
        label,
        os,
    } = receive(channel, &sealed)?
    else {
        return Err(
            "The new device said something out of turn, so the link was stopped.".to_owned(),
        );
    };
    let new_device =
        DeviceId::parse(&device_id).map_err(|_| super::super::UNREADABLE.to_owned())?;
    let new_key = unb64(&signature_key)
        .filter(|key| key.len() == e2ee::identity::SIGNATURE_KEY_LEN && key[0] == 4)
        .ok_or(super::super::UNREADABLE)?;
    let (dir, cache_key, verified, from_device, user, most) = {
        let device = lock(device);
        (
            device.dir.clone(),
            device
                .cache_key
                .clone()
                .ok_or("Encryption isn't set up on this device.")?,
            device.notes.verified.clone(),
            device
                .engine
                .as_ref()
                .map(|engine| engine.device_id().to_string())
                .unwrap_or_default(),
            device.saved.user_id.clone(),
            device.link_max,
        )
    };
    let now = now_secs();
    let history = CHOICES
        .iter()
        .map(|scope| {
            let (messages, bytes) = size_of(&dir, &cache_key, *scope, now);
            E2eeHistory {
                history: scope.to_string(),
                messages,
                bytes,
            }
        })
        .collect();
    let check = channel.check_digits().to_owned();
    let print = fingerprint(&new_key);
    job.show(|view| {
        view.stage = LinkStage::Confirm;
        view.check = check;
        view.label = label.chars().filter(|c| !c.is_control()).take(80).collect();
        view.os = os.chars().filter(|c| !c.is_control()).take(40).collect();
        view.fingerprint = print;
        view.history = history;
    });

    let scope = job.confirmed()?;
    let cancelled = || job.stopped() || Instant::now() >= job.deadline;
    if cancelled() {
        return Err(STOPPED.to_owned());
    }
    job.show(|view| {
        view.stage = LinkStage::OwnerApproval;
        view.check.clear();
    });
    // Native approval is a core-side gate, including for direct LinkConfirm IPC callers.
    // Neither the device lock nor the link state lock is held while the person authenticates.
    super::approval::verify(
        job.approval.as_ref(),
        "Approve adding a device to your Tiny account",
        &cancelled,
    )?;
    if cancelled() {
        return Err(STOPPED.to_owned());
    }
    job.show(|view| {
        view.stage = LinkStage::Sending;
        // The digits were typed: they are not shown again.
        view.check.clear();
    });
    let cross_signature = {
        let mut device = lock(device);
        if cancelled() {
            return Err(STOPPED.to_owned());
        }
        let signature = device
            .engine()?
            .cross_sign(new_device, &new_key)
            .map_err(|err| format!("It couldn't be approved: {err}"))?;
        // Added here, by the person: no "New device added" notice on this device.
        device.notes.mine.insert(device_id.clone());
        device.save_notes();
        signature
    };
    let archive = match scope {
        Scope::None => None,
        scope => Some(send_archive(
            job,
            relay,
            link,
            &code.link_id,
            &dir,
            &cache_key,
            &verified,
            most,
            Meta {
                v: archive::VERSION,
                user,
                from_device,
                created: now_secs().to_string(),
                scope,
            },
        )?),
    };
    let approve = Message::Approve {
        cross_signature: b64(&cross_signature),
        history: scope,
        archive,
    };
    if cancelled() {
        return Err(STOPPED.to_owned());
    }
    relay.post(link, Role::Old, &channel.send(&approve).map_err(words)?)?;
    let sealed = next_from(job, relay, link, Role::New, &mut after)?;
    match receive(channel, &sealed)? {
        Message::Done {} => {}
        _ => {
            return Err(
                "The new device said something out of turn, so the link was stopped.".to_owned(),
            )
        }
    }
    let mut device = lock(device);
    let me = device.saved.user_id.clone();
    let _ = device.verify_user(&me);
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// Whether a cached message goes with `scope`. One kept before messages were dated goes only with
/// "all".
fn within(opened: &Opened, scope: Scope, now: i64) -> bool {
    match scope {
        Scope::None => false,
        Scope::All => true,
        Scope::Days(days) => opened.at > 0 && opened.at >= now - i64::from(days) * 86_400,
    }
}

/// The conversations this folder has a cache for.
fn cached_conversations(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let conv = name
                .strip_prefix("cache-")?
                .strip_suffix(".log")?
                .to_owned();
            super::super::safe_id(&conv).then_some(conv)
        })
        .collect();
    out.sort();
    out
}

/// How many cached messages `scope` sends, and about how many bytes. Read from disk, without the
/// device's lock.
pub(super) fn size_of(dir: &Path, key: &CacheKey, scope: Scope, now: i64) -> (u64, u64) {
    let (mut messages, mut bytes) = (0u64, 0u64);
    let Ok(rows) = super::export_rows::Rows::scan(dir, cached_conversations(dir), key, |opened| {
        within(opened, scope, now)
    }) else {
        return (0, 0);
    };
    if rows
        .visit(key, |conv, id, opened| {
            messages += 1;
            bytes += serde_json::to_vec(&opened).map_or(0, |json| json.len() as u64)
                + (conv.len() + id.len() + 40) as u64;
            Ok(true)
        })
        .is_err()
    {
        return (0, 0);
    }
    // The archive's own records and the tags of its chunks.
    (messages, bytes + bytes / 4096 + 256)
}

/// Packs what `scope` sends, uploads it, and answers what the `approve` message says of it.
#[allow(clippy::too_many_arguments)]
fn send_archive(
    job: &Linking,
    relay: &dyn Relay,
    link: &str,
    link_id: &[u8; 16],
    dir: &Path,
    cache_key: &CacheKey,
    verified: &std::collections::BTreeMap<String, String>,
    most: u64,
    meta: Meta,
) -> Result<ArchiveRef, String> {
    let key = ArchiveKey::generate().map_err(|_| "No randomness is available.".to_owned())?;
    let path = dir.join(ARCHIVE_FILE);
    let result = (|| {
        let (summary, left_out) =
            write_archive(&path, &key, link_id, dir, cache_key, verified, most, &meta)
                .map_err(|err| format!("The history couldn't be packed: {err}"))?;
        if summary.bytes > most {
            return Err("The history is more than this server takes. Send fewer days.".to_owned());
        }
        job.show(|view| {
            view.total = summary.bytes;
            view.left_out = left_out;
        });
        relay.put_archive(link, &path, summary.bytes)?;
        job.show(|view| view.done = summary.bytes);
        Ok(ArchiveRef {
            key: b64(key.as_bytes()),
            bytes: summary.bytes,
            sha256: b64(&summary.sha256),
            chunks: summary.chunks,
        })
    })();
    let _ = std::fs::remove_file(&path);
    result
}

/// The size of an archive whose records come to `plain` bytes: the header, and a tag per chunk.
fn sealed_size(plain: u64) -> u64 {
    archive::HEADER_LEN as u64 + plain + 16 * (plain / archive::CHUNK as u64 + 1)
}

/// Writes the archive of what `meta.scope` sends, newest first, and stops before it would be
/// larger than `most` bytes (the server's `e2ee_link_archive_max`). Answers how many messages
/// were left out for that.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_archive(
    path: &Path,
    key: &ArchiveKey,
    link_id: &[u8],
    dir: &Path,
    cache_key: &CacheKey,
    verified: &std::collections::BTreeMap<String, String>,
    most: u64,
    meta: &Meta,
) -> e2ee::Result<(archive::Summary, u64)> {
    let file = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|err| e2ee::Error::Io(err.to_string()))?,
    );
    let (mut file, summary, left_out) =
        pack(file, key, link_id, dir, cache_key, verified, most, meta)?;
    file.flush()
        .map_err(|err| e2ee::Error::Io(err.to_string()))?;
    file.get_ref()
        .sync_all()
        .map_err(|err| e2ee::Error::Io(err.to_string()))?;
    Ok((summary, left_out))
}

/// [`write_archive`] into any writer, which it answers back with the summary.
#[allow(clippy::too_many_arguments)]
pub(super) fn pack<W: Write>(
    out: W,
    key: &ArchiveKey,
    link_id: &[u8],
    dir: &Path,
    cache_key: &CacheKey,
    verified: &std::collections::BTreeMap<String, String>,
    most: u64,
    meta: &Meta,
) -> e2ee::Result<(W, archive::Summary, u64)> {
    let mut writer = Writer::new(key, link_id, meta, out)?;
    let now = now_secs();
    let rows =
        super::export_rows::Rows::scan(dir, cached_conversations(dir), cache_key, |opened| {
            within(opened, meta.scope, now)
        })?;
    let record = |json: &Value| serde_json::to_vec(json).map_or(0, |bytes| bytes.len() as u64 + 4);
    let mut tail = record(&json!({ "r": "end", "msgs": u64::MAX }));
    for (user, number) in verified {
        tail += record(&json!({ "r": "verified", "user": user, "number": number }));
    }
    let mut plain =
        record(&serde_json::to_value(archive::Record::Meta(meta.clone())).unwrap_or(Value::Null));
    let total = rows.total;
    let mut sent = 0u64;
    rows.visit(cache_key, |conv, id, mut opened| {
        opened.imported_from = None;
        let value = serde_json::to_value(&opened).map_err(|_| e2ee::Error::Malformed("cache"))?;
        let size = record(&json!({ "r": "msg", "conv": conv, "id": id, "opened": value }));
        if sealed_size(plain + size + tail) > most {
            return Ok(false);
        }
        plain += size;
        writer.message(conv, id, &value)?;
        sent += 1;
        Ok(true)
    })?;
    for (user, number) in verified {
        writer.verified(user, number)?;
    }
    let (out, summary) = writer.finish()?;
    Ok((out, summary, total - sent))
}

/// The relay and the device routes over HTTP, as this session.
struct Http {
    saved: Saved,
}

impl Http {
    fn path(link: &str, rest: &str) -> String {
        format!("/v1/e2ee/links/{link}{rest}")
    }

    fn url(&self, path: &str) -> String {
        super::super::endpoint(&self.saved.base_url, path)
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.saved.token)
    }

    fn asked(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, Value), Failed> {
        super::super::asked(method, &self.saved, path, body)
            .map_err(|why| Failed::Words(why.to_owned()))
    }
}

fn refused(status: u16, answer: &Value) -> Failed {
    match status {
        404 | 410 => Failed::Gone,
        429 => {
            Failed::Words("Too many links at once. Wait a few minutes and try again.".to_owned())
        }
        413 => Failed::Words(
            "That history is more than this server takes. Send fewer days.".to_owned(),
        ),
        status => Failed::Words(
            super::super::explain(status, answer, super::super::Missing::Plain).to_owned(),
        ),
    }
}

impl Relay for Http {
    fn publish_keys(
        &self,
        device_id: &str,
        packages: &[Vec<u8>],
        last_resort: &[u8],
    ) -> Result<(), Failed> {
        let body = json!({"device_id":device_id,
            "key_packages":packages.iter().map(|key| b64(key)).collect::<Vec<_>>(),
            "last_resort":b64(last_resort)});
        match self.asked("POST", "/v1/e2ee/key_packages", Some(body))? {
            (200 | 204, _) => Ok(()),
            (status, _) => Err(Failed::Words(format!("The device was approved, but its encryption setup did not finish ({status}). Try linking again."))),
        }
    }
    fn create(&self) -> Result<(String, String), Failed> {
        match self.asked("POST", "/v1/e2ee/links", None)? {
            (200 | 201, answer) => {
                let id = answer.get("link_id").and_then(Value::as_str).unwrap_or("");
                let expires = answer
                    .get("expires_at")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Ok((id.to_owned(), expires.to_owned()))
            }
            (404, _) => Err(Failed::Words(
                "This server can't link devices yet.".to_owned(),
            )),
            (status, answer) => Err(refused(status, &answer)),
        }
    }

    fn post(&self, link: &str, from: Role, data: &[u8]) -> Result<(), Failed> {
        let body = json!({ "from": from.as_str(), "data": b64(data) });
        match self.asked("POST", &Self::path(link, "/messages"), Some(body))? {
            (200 | 201, _) => Ok(()),
            (status, answer) => Err(refused(status, &answer)),
        }
    }

    fn read(
        &self,
        link: &str,
        after: i64,
        wait: u64,
    ) -> Result<Vec<(i64, String, Vec<u8>)>, Failed> {
        let path = Self::path(link, &format!("/messages?after={after}&wait={wait}"));
        let (status, answer) = super::super::call_with_timeout(
            "GET",
            &self.url(&path),
            Some(&self.saved.token),
            None,
            Duration::from_secs(wait + 20),
        )
        .map_err(|_| Failed::Words(super::super::UNREACHABLE.to_owned()))?;
        if status != 200 {
            return Err(refused(status, &answer));
        }
        let rows = answer
            .as_array()
            .ok_or(Failed::Words(super::super::UNREADABLE.to_owned()))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.get("seq")?.as_i64()?,
                    row.get("from")?.as_str()?.to_owned(),
                    unb64(row.get("data")?.as_str()?)?,
                ))
            })
            .collect())
    }

    fn put_archive(&self, link: &str, path: &Path, bytes: u64) -> Result<(), Failed> {
        let mut file = std::fs::File::open(path).map_err(|err| Failed::Words(err.to_string()))?;
        let sent = super::super::agent_with_timeout(Duration::from_secs(60 * 60))
            .put(&self.url(&Self::path(link, "/archive")))
            .header("Authorization", &self.bearer())
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", &bytes.to_string())
            .send(ureq::SendBody::from_reader(&mut file));
        let mut response = sent.map_err(|_| Failed::Words(super::super::UNREACHABLE.to_owned()))?;
        match response.status().as_u16() {
            200 | 201 | 204 => Ok(()),
            status => {
                let answer = response.body_mut().read_json().unwrap_or(Value::Null);
                Err(refused(status, &answer))
            }
        }
    }

    fn get_archive(
        &self,
        link: &str,
        to: &Path,
        max: u64,
        moved: &dyn Fn(u64),
    ) -> Result<u64, Failed> {
        let sent = super::super::agent_with_timeout(Duration::from_secs(60 * 60))
            .get(&self.url(&Self::path(link, "/archive")))
            .header("Authorization", &self.bearer())
            .call();
        let mut response = sent.map_err(|_| Failed::Words(super::super::UNREACHABLE.to_owned()))?;
        let status = response.status().as_u16();
        if status != 200 {
            let answer = response.body_mut().read_json().unwrap_or(Value::Null);
            return Err(refused(status, &answer));
        }
        let mut file = std::fs::File::create(to).map_err(|err| Failed::Words(err.to_string()))?;
        let mut reader = response.body_mut().as_reader().take(max + 1);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        loop {
            let n = reader
                .read(&mut buffer)
                .map_err(|_| Failed::Words(super::super::UNREACHABLE.to_owned()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n])
                .map_err(|err| Failed::Words(err.to_string()))?;
            done += n as u64;
            moved(done);
        }
        file.sync_all()
            .map_err(|err| Failed::Words(err.to_string()))?;
        Ok(done)
    }

    fn delete(&self, link: &str) {
        let _ = self.asked("DELETE", &Self::path(link, ""), None);
    }

    fn register(
        &self,
        device_id: &str,
        signature_key: &[u8],
        cross_signature: &[u8],
    ) -> Result<(), Failed> {
        let body = json!({
            "signature_key": b64(signature_key),
            "cross_signature": b64(cross_signature),
            "label": device_label(),
        });
        match self.asked("PUT", &format!("/v1/e2ee/devices/{device_id}"), Some(body))? {
            // 409: registered already, by an earlier try of this link.
            (200 | 201 | 409, _) => Ok(()),
            (status, answer) => Err(refused(status, &answer)),
        }
    }

    fn conversations(&self) -> Result<BTreeSet<String>, Failed> {
        super::super::messages::conversation_ids(&self.saved)
            .ok_or_else(|| Failed::Words(super::super::UNREADABLE.to_owned()))
    }
}

#[cfg(test)]
mod tests;
