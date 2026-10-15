//! The voice message or audio file playing, across every network: what the shell is told
//! (`Change::Playback`) and the `mediad --play` that plays it. chatd decodes nothing here; it
//! starts mediad for a file when the user presses play and closes its stdin on pause, so a
//! paused or finished file holds no process at all.
//!
//! Only a file inside the media cache is handed to mediad: the shell names the path, and a path
//! anywhere else is refused, so the socket cannot be used to make mediad read other files.

use crate::hub::{lock, Hub};
use crate::media::mediad_path;
use protocol::{
    read_frame, write_frame, AccountKey, Change, Key, PlayRequest, PlayState, Playback, PlayerEvent,
    PlayerRequest, RATES,
};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// What the shell shows when mediad could not be run, or died while playing.
const NO_PLAYER: &str = "Playing stopped. Try again.";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[derive(Default)]
struct Slot {
    shown: Option<Playback>,
    /// The file's place on disk and the speaker asked for, to start again after a pause.
    path: PathBuf,
    output: Option<String>,
    /// The running mediad's stdin. Closing it is the pause.
    stdin: Option<ChildStdin>,
    /// Tells this start from the next, for the threads that read an earlier mediad.
    generation: u64,
}

#[derive(Default)]
pub struct Player {
    slot: Mutex<Slot>,
}

/// True when `path` is a file inside `media`, after following links.
pub fn inside(media: &Path, path: &Path) -> bool {
    match (media.canonicalize(), path.canonicalize()) {
        (Ok(media), Ok(path)) => path.starts_with(media) && path.is_file(),
        _ => false,
    }
}

impl Player {
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
                if !inside(media, &path) {
                    return Err("That file is not in tinyclient's media cache.".to_owned());
                }
                self.start(hub, Start { account, file, path, conversation, message, title, at, rate, output });
            }
            PlayRequest::Pause => self.pause(hub),
            PlayRequest::Seek { at } => self.seek(hub, at),
            PlayRequest::Rate { rate } => self.rate(hub, rate),
            PlayRequest::Stop => self.stop(hub),
        }
        Ok(())
    }

    fn start(&self, hub: &Arc<Hub>, start: Start) {
        let mut slot = lock(&self.slot);
        // Whatever was playing stops; its mediad exits on its own.
        slot.stdin = None;
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
        });
        slot.path = start.path;
        slot.output = start.output;
        match self.spawn(hub, &slot, generation) {
            Ok(stdin) => slot.stdin = Some(stdin),
            Err(err) => {
                eprintln!("chatd: the player did not start: {err}");
                if let Some(shown) = &mut slot.shown {
                    shown.state = PlayState::Failed { detail: NO_PLAYER.to_owned() };
                }
            }
        }
        drop(slot);
        self.tell(hub);
    }

    /// Runs `mediad --play` for the slot's file, from the slot's place.
    fn spawn(&self, hub: &Arc<Hub>, slot: &Slot, generation: u64) -> io::Result<ChildStdin> {
        let shown = slot.shown.as_ref().expect("a file to play");
        let mut child = protocol::launch::no_console(Command::new(mediad_path()?).arg("--play")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
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
    fn heard(&self, hub: &Hub, generation: u64, event: PlayerEvent) {
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
            PlayerEvent::Ended { duration } => {
                shown.state = PlayState::Ended;
                shown.duration = shown.duration.max(duration);
                shown.at = shown.duration;
                slot.stdin = None;
            }
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
                eprintln!("chatd: the player exited (clean: {clean}) while playing");
                shown.state = PlayState::Failed { detail: NO_PLAYER.to_owned() };
            }
        }
        drop(slot);
        self.tell(hub);
    }

    fn pause(&self, hub: &Hub) {
        let mut slot = lock(&self.slot);
        slot.stdin = None;
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
        slot.generation += 1;
        drop(slot);
        if had {
            self.tell(hub);
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
