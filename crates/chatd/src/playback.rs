//! The voice message or audio file playing, across every network: what the shell is told
//! (`Change::Playback`) and the `mediad --play` that plays it. chatd decodes nothing here; it
//! starts mediad for a file when the user presses play and closes its stdin on pause, so a
//! paused or finished file holds no process at all.
//!
//! Only a file inside the media cache or the recorder's current finished note is handed to
//! mediad, so the socket cannot be used to make mediad read other files.

use crate::hub::{lock, Hub};
use crate::media::mediad_path;
use protocol::{
    read_frame, write_frame, AccountKey, Attachment, Change, Key, MediaFile, MediaKind, Message,
    MessageKind, Page, PlayRequest, PlayState, Playback, PlayerEvent, PlayerRequest, RATES,
};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

/// What the shell shows when mediad could not be run, or died while playing.
const NO_PLAYER: &str = "Playing stopped. Try again.";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// First playable voice or round video attachment on `message`, excluding view-once media
/// (which must never be auto-started).
pub(crate) fn playable_attachment(message: &Message) -> Option<&Attachment> {
    if message.deleted || message.kind != MessageKind::Normal {
        return None;
    }
    message.attachments.iter().find(|attachment| {
        !attachment.view_once
            && ((attachment.file.kind == MediaKind::Audio
                && (!attachment.waveform.is_empty()
                    || attachment.name == "Voice message"
                    || attachment.mime == "audio/ogg"))
                || attachment.round)
    })
}

/// Looks up the immediately adjacent message (`forward == true` for newer, `false` for older)
/// in `conversation` and returns it only if it is a consecutive voice or round message.
fn adjacent_playable(
    hub: &Hub,
    conversation: &Key,
    message_id: &str,
    forward: bool,
) -> Option<(Message, Attachment)> {
    if message_id.is_empty() {
        return None;
    }
    let page = if forward {
        Page::After { id: message_id.to_owned() }
    } else {
        Page::Before { id: message_id.to_owned() }
    };
    let found = hub.store().messages(conversation, &page, 1, crate::hub::now()).ok()??;
    let candidate = found.messages.into_iter().next()?;
    let attachment = playable_attachment(&candidate)?.clone();
    Some((candidate, attachment))
}

struct WaitingFile {
    hub: Weak<Hub>,
    account: AccountKey,
    file: String,
    conversation: Key,
    message: String,
    title: String,
    rate: u16,
    output: Option<String>,
    media_dir: PathBuf,
    generation: u64,
}

#[derive(Default)]
struct Hooks {
    on_started: Option<Arc<dyn Fn(&Key, &str) + Send + Sync>>,
    on_fetch: Option<Arc<dyn Fn(&AccountKey, &str) + Send + Sync>>,
}

#[derive(Default)]
struct Slot {
    shown: Option<Playback>,
    /// The file's place on disk and the speaker asked for, to start again after a pause.
    path: PathBuf,
    /// Media cache directory used for validating auto-play files.
    media_dir: PathBuf,
    output: Option<String>,
    /// The running mediad's stdin. Closing it is the pause.
    stdin: Option<Box<dyn Write + Send>>,
    /// Tells this start from the next, for the threads that read an earlier mediad.
    generation: u64,
    /// Next/previous playable file being fetched before playback begins.
    waiting: Option<WaitingFile>,
}

#[derive(Default)]
pub struct Player {
    slot: Mutex<Slot>,
    hooks: Mutex<Hooks>,
}

/// True when `path` is a file inside `media`, after following links.
pub fn inside(media: &Path, path: &Path) -> bool {
    match (media.canonicalize(), path.canonicalize()) {
        (Ok(media), Ok(path)) => path.starts_with(media) && path.is_file(),
        _ => false,
    }
}

impl Player {
    /// Registers a callback invoked whenever a message starts playing (used to mark listened).
    pub fn on_started(&self, hook: impl Fn(&Key, &str) + Send + Sync + 'static) {
        lock(&self.hooks).on_started = Some(Arc::new(hook));
    }

    /// Registers a callback invoked when auto-play or next/previous needs a file fetched first.
    pub fn on_fetch(&self, hook: impl Fn(&AccountKey, &str) + Send + Sync + 'static) {
        lock(&self.hooks).on_fetch = Some(Arc::new(hook));
    }

    fn notify_started(&self, conversation: &Key, message_id: &str) {
        if message_id.is_empty() {
            return;
        }
        let hook = lock(&self.hooks).on_started.clone();
        if let Some(hook) = hook {
            hook(conversation, message_id);
        }
    }

    /// What the shell shows now.
    pub fn shown(&self) -> Option<Playback> {
        lock(&self.slot).shown.clone()
    }

    fn tell(&self, hub: &Hub) {
        hub.changed(Change::Playback { playback: self.shown() });
    }

    /// The shell's request. Errors are words for the user.
    pub fn request(&self, hub: &Arc<Hub>, media: &Path, request: PlayRequest) -> Result<(), String> {
        match request {
            PlayRequest::Start { account, file, path, conversation, message, title, at, rate, output } => {
                let path = PathBuf::from(path);
                let preview = hub.recorder.shown().is_some_and(|recording| {
                    recording.conversation == conversation && recording.conversation.account() == account
                        && matches!(recording.state, protocol::RecordState::Recorded { path: recorded, .. }
                            if Path::new(&recorded) == path && inside(&crate::blobs::upload::outbox(), &path))
                });
                if !inside(media, &path) && !preview {
                    return Err("That file is not in tinyclient's media cache.".to_owned());
                }
                self.start(hub, media.to_path_buf(), Start { account, file, path, conversation, message, title, at, rate, output });
            }
            PlayRequest::Pause => self.pause(hub),
            PlayRequest::Seek { at } => self.seek(hub, at),
            PlayRequest::Rate { rate } => self.rate(hub, rate),
            PlayRequest::Next => self.step(hub, media, true),
            PlayRequest::Previous => self.step(hub, media, false),
            PlayRequest::Stop => self.stop(hub),
        }
        Ok(())
    }

    fn start(&self, hub: &Arc<Hub>, media_dir: PathBuf, start: Start) {
        let has_next = adjacent_playable(hub, &start.conversation, &start.message, true).is_some();
        let has_previous = adjacent_playable(hub, &start.conversation, &start.message, false).is_some();
        let started_conv = start.conversation.clone();
        let started_msg = start.message.clone();
        let mut slot = lock(&self.slot);
        // Whatever was playing stops; its mediad exits on its own.
        slot.stdin = None;
        slot.waiting = None;
        slot.generation += 1;
        let generation = slot.generation;
        let same = slot.shown.as_ref().filter(|shown| shown.is(&start.account, &start.file));
        // Pressing play on a paused file goes on from where it was; on an ended one, from the top.
        let at = start.at.unwrap_or_else(|| match same {
            Some(shown) if shown.state != PlayState::Ended => shown.at,
            _ => 0,
        });
        let duration = same.map_or(0, |shown| shown.duration);
        let rate = if RATES.contains(&start.rate) { start.rate } else { 100 };
        slot.shown = Some(Playback {
            account: start.account,
            file: start.file,
            conversation: start.conversation,
            message: start.message,
            title: start.title,
            state: PlayState::Loading,
            at,
            since: now_ms(),
            duration,
            rate,
            has_next,
            has_previous,
        });
        slot.path = start.path;
        slot.media_dir = media_dir;
        slot.output = start.output;
        let ok = match self.spawn(hub, &slot, generation) {
            Ok(stdin) => {
                slot.stdin = Some(stdin);
                true
            }
            Err(err) => {
                tracing::warn!("chatd: the player did not start: {err}");
                if let Some(shown) = &mut slot.shown {
                    shown.state = PlayState::Failed { detail: NO_PLAYER.to_owned() };
                }
                false
            }
        };
        drop(slot);
        self.tell(hub);
        if ok {
            self.notify_started(&started_conv, &started_msg);
        }
    }

    /// Starts playing an adjacent playable message, or requests its file from the adapter first.
    /// Returns `true` if playback started or is waiting on a fetch.
    fn begin_message(
        &self,
        hub: &Arc<Hub>,
        media_dir: &Path,
        conversation: &Key,
        message: Message,
        attachment: Attachment,
        rate: u16,
        output: Option<String>,
    ) -> bool {
        let title = if !message.sender.name.is_empty() {
            message.sender.name.clone()
        } else {
            attachment.name.clone()
        };
        if let Some(path) = attachment.file.path.as_deref() {
            let path = PathBuf::from(path);
            if inside(media_dir, &path) {
                self.start(
                    hub,
                    media_dir.to_path_buf(),
                    Start {
                        account: conversation.account(),
                        file: attachment.file.id,
                        path,
                        conversation: conversation.clone(),
                        message: message.id,
                        title,
                        at: Some(0),
                        rate,
                        output,
                    },
                );
                return true;
            }
        }
        let fetch = lock(&self.hooks).on_fetch.clone();
        let Some(fetch) = fetch else {
            return false;
        };
        let has_next = adjacent_playable(hub, conversation, &message.id, true).is_some();
        let has_previous = adjacent_playable(hub, conversation, &message.id, false).is_some();
        let account = conversation.account();
        let file_id = attachment.file.id.clone();
        let rate = if RATES.contains(&rate) { rate } else { 100 };
        let mut slot = lock(&self.slot);
        slot.stdin = None;
        slot.generation += 1;
        let generation = slot.generation;
        slot.media_dir = media_dir.to_path_buf();
        slot.output = output.clone();
        slot.shown = Some(Playback {
            account: account.clone(),
            file: file_id.clone(),
            conversation: conversation.clone(),
            message: message.id.clone(),
            title: title.clone(),
            state: PlayState::Loading,
            at: 0,
            since: now_ms(),
            duration: attachment.duration.saturating_mul(1_000),
            rate,
            has_next,
            has_previous,
        });
        slot.waiting = Some(WaitingFile {
            hub: Arc::downgrade(hub),
            account: account.clone(),
            file: file_id.clone(),
            conversation: conversation.clone(),
            message: message.id,
            title,
            rate,
            output,
            media_dir: media_dir.to_path_buf(),
            generation,
        });
        drop(slot);
        self.tell(hub);
        fetch(&account, &file_id);
        true
    }

    /// Called when a fetched media file lands on disk or fails; starts any waiting auto-play.
    pub fn file_landed(&self, account: &AccountKey, file: &MediaFile) {
        let waiting = {
            let mut slot = lock(&self.slot);
            let matches = slot.waiting.as_ref().is_some_and(|w| &w.account == account && w.file == file.id);
            if !matches {
                return;
            }
            slot.waiting.take()
        };
        let Some(waiting) = waiting else {
            return;
        };
        let Some(hub) = waiting.hub.upgrade() else {
            return;
        };
        if let Some(path) = file.path.as_deref() {
            let path = PathBuf::from(path);
            if inside(&waiting.media_dir, &path) {
                let still_current = lock(&self.slot).generation == waiting.generation;
                if still_current {
                    self.start(
                        &hub,
                        waiting.media_dir,
                        Start {
                            account: waiting.account,
                            file: waiting.file,
                            path,
                            conversation: waiting.conversation,
                            message: waiting.message,
                            title: waiting.title,
                            at: Some(0),
                            rate: waiting.rate,
                            output: waiting.output,
                        },
                    );
                }
                return;
            }
        }
        if let Some(reason) = file.failed.as_ref() {
            let mut slot = lock(&self.slot);
            if slot.generation == waiting.generation {
                if let Some(shown) = &mut slot.shown {
                    shown.state = PlayState::Failed { detail: reason.clone() };
                }
                drop(slot);
                self.tell(&hub);
            }
        }
    }

    fn step(&self, hub: &Arc<Hub>, media: &Path, forward: bool) {
        let current = {
            let slot = lock(&self.slot);
            slot.shown.as_ref().map(|shown| {
                (shown.conversation.clone(), shown.message.clone(), shown.rate, slot.output.clone())
            })
        };
        let Some((conversation, message, rate, output)) = current else {
            return;
        };
        if let Some((target, attachment)) = adjacent_playable(hub, &conversation, &message, forward) {
            self.begin_message(hub, media, &conversation, target, attachment, rate, output);
        }
    }

    /// Runs `mediad --play` for the slot's file, from the slot's place.
    fn spawn(&self, hub: &Arc<Hub>, slot: &Slot, generation: u64) -> io::Result<Box<dyn Write + Send>> {
        let shown = slot.shown.as_ref().expect("a file to play");
        // The process on a desktop; mediad's threads in this process on a phone.
        let spawned = protocol::media_host::spawn(protocol::launch::no_console(Command::new(mediad_path()?).arg("--play")))?;
        let protocol::media_host::Spawned { mut child, stdin, stdout } = spawned;
        let mut stdin = stdin.expect("stdin is piped");
        let mut stdout = BufReader::new(stdout.expect("stdout is piped"));
        let open = PlayerRequest::Open {
            path: slot.path.display().to_string(),
            at: shown.at,
            rate: shown.rate,
            output: slot.output.clone(),
        };
        write_frame(&mut stdin, &open)?;
        let hub = hub.clone();
        std::thread::Builder::new().name("player".into()).stack_size(128 * 1024).spawn(move || {
            // Ends when mediad exits, which it does when the file ends or its stdin closes.
            while let Ok(event) = read_frame::<PlayerEvent>(&mut stdout) {
                hub.player.heard(&hub, generation, event);
            }
            let clean = child.wait().is_ok_and(|status| status.success());
            hub.player.gone(&hub, generation, clean);
        })?;
        Ok(stdin)
    }

    /// What a mediad said, if it is still the one playing.
    fn heard(&self, hub: &Arc<Hub>, generation: u64, event: PlayerEvent) {
        if let PlayerEvent::Ended { duration } = event {
            let advance = {
                let slot = lock(&self.slot);
                if slot.generation != generation {
                    return;
                }
                let paused = slot.stdin.is_none();
                let Some(shown) = &slot.shown else {
                    return;
                };
                if paused {
                    None
                } else {
                    Some((
                        shown.conversation.clone(),
                        shown.message.clone(),
                        shown.rate,
                        slot.output.clone(),
                        slot.media_dir.clone(),
                    ))
                }
            };
            if let Some((conversation, message, rate, output, media_dir)) = advance {
                if let Some((next_msg, attachment)) = adjacent_playable(hub, &conversation, &message, true) {
                    let still_active = {
                        let slot = lock(&self.slot);
                        slot.generation == generation && slot.stdin.is_some()
                    };
                    if still_active && self.begin_message(hub, &media_dir, &conversation, next_msg, attachment, rate, output) {
                        return;
                    }
                }
            }
            let mut slot = lock(&self.slot);
            if slot.generation != generation {
                return;
            }
            if let Some(shown) = &mut slot.shown {
                shown.state = PlayState::Ended;
                shown.duration = shown.duration.max(duration);
                shown.at = shown.duration;
                slot.stdin = None;
            }
            drop(slot);
            self.tell(hub);
            return;
        }
        let mut slot = lock(&self.slot);
        if slot.generation != generation {
            return;
        }
        let paused = slot.stdin.is_none();
        let Some(shown) = &mut slot.shown else {
            return;
        };
        match event {
            // A pause asked for while it was starting wins.
            PlayerEvent::Playing { .. } if paused => return,
            PlayerEvent::Playing { at, duration, rate } => {
                shown.state = PlayState::Playing;
                shown.at = at;
                shown.since = now_ms();
                shown.rate = rate;
                if duration > 0 {
                    shown.duration = duration;
                }
            }
            PlayerEvent::Ended { .. } => unreachable!(),
            // Where a pause really left it, a moment after the shell was told the clock's guess.
            PlayerEvent::Stopped { at } => {
                if shown.state == PlayState::Paused {
                    shown.at = at;
                }
            }
            PlayerEvent::Failed { detail } => {
                shown.state = PlayState::Failed { detail };
                slot.stdin = None;
            }
        }
        drop(slot);
        self.tell(hub);
    }

    /// A mediad exited. One that died while it was meant to be playing failed.
    fn gone(&self, hub: &Hub, generation: u64, clean: bool) {
        let mut slot = lock(&self.slot);
        if slot.generation != generation || slot.stdin.is_none() {
            return;
        }
        slot.stdin = None;
        if let Some(shown) = &mut slot.shown {
            if matches!(shown.state, PlayState::Loading | PlayState::Playing) {
                tracing::warn!("chatd: the player exited (clean: {clean}) while playing");
                shown.state = PlayState::Failed { detail: NO_PLAYER.to_owned() };
            }
        }
        drop(slot);
        self.tell(hub);
    }

    fn pause(&self, hub: &Hub) {
        let mut slot = lock(&self.slot);
        slot.stdin = None;
        slot.waiting = None;
        let Some(shown) = &mut slot.shown else {
            return;
        };
        if matches!(shown.state, PlayState::Loading | PlayState::Playing) {
            shown.at = shown.position(now_ms());
            shown.since = now_ms();
            shown.state = PlayState::Paused;
        }
        drop(slot);
        self.tell(hub);
    }

    fn seek(&self, hub: &Hub, at: u32) {
        let mut slot = lock(&self.slot);
        let Some(shown) = &mut slot.shown else {
            return;
        };
        let at = if shown.duration > 0 { at.min(shown.duration) } else { at };
        shown.at = at;
        shown.since = now_ms();
        if shown.state == PlayState::Ended {
            shown.state = PlayState::Paused;
        }
        // Playing: mediad jumps and says so; until then the bar is already where it was put.
        if let Some(stdin) = &mut slot.stdin {
            if write_frame(stdin, &PlayerRequest::Seek { at }).is_err() {
                slot.stdin = None;
            }
        }
        drop(slot);
        self.tell(hub);
    }

    fn rate(&self, hub: &Hub, rate: u16) {
        if !RATES.contains(&rate) {
            return;
        }
        let mut slot = lock(&self.slot);
        let Some(shown) = &mut slot.shown else {
            return;
        };
        let now = now_ms();
        shown.at = shown.position(now);
        shown.since = now;
        shown.rate = rate;
        if let Some(waiting) = &mut slot.waiting {
            waiting.rate = rate;
        }
        if let Some(stdin) = &mut slot.stdin {
            if write_frame(stdin, &PlayerRequest::Rate { rate }).is_err() {
                slot.stdin = None;
            }
        }
        drop(slot);
        self.tell(hub);
    }

    pub fn stop(&self, hub: &Hub) {
        let mut slot = lock(&self.slot);
        let had = slot.shown.take().is_some();
        slot.stdin = None;
        slot.waiting = None;
        slot.generation += 1;
        drop(slot);
        if had {
            self.tell(hub);
        }
    }

    /// A staged recording is being sent, removed, or replaced. Release its player first.
    pub(crate) fn stop_file(&self, hub: &Hub, path: &Path) {
        let playing = lock(&self.slot).path == path;
        if playing {
            self.stop(hub);
        }
    }

    /// Sign-out: a file of that account stops, since its cache is about to go.
    pub fn forget(&self, hub: &Hub, account: &AccountKey) {
        let playing = lock(&self.slot).shown.as_ref().is_some_and(|shown| &shown.account == account);
        if playing {
            self.stop(hub);
        }
    }
}

struct Start {
    account: AccountKey,
    file: String,
    path: PathBuf,
    conversation: Key,
    message: String,
    title: String,
    at: Option<u32>,
    rate: u16,
    output: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_file_inside_the_cache_is_played() {
        let dir = std::env::temp_dir().join(format!("tinyclient-inside-{}", std::process::id()));
        let media = dir.join("media");
        std::fs::create_dir_all(media.join("discord/me")).unwrap();
        std::fs::write(media.join("discord/me/voice"), b"x").unwrap();
        std::fs::write(dir.join("secret"), b"x").unwrap();
        assert!(inside(&media, &media.join("discord/me/voice")));
        assert!(!inside(&media, &dir.join("secret")));
        assert!(!inside(&media, &media.join("discord/me/../../secret")), "climbing out is refused");
        assert!(!inside(&media, &media.join("discord/me")), "a directory is not a file");
        assert!(!inside(&media, &media.join("discord/me/missing")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
