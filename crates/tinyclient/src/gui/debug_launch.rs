//! A debug build's launch request: open a Tiny room and join its call, optionally with the
//! camera on and the sound on the speaker, through the same calls a tap on the room and on "Join
//! channel" makes. For end-to-end call tests on machines where nobody can tap: Android's debug
//! launch extras (`am start -n dev.tinyclient.app/.TinyActivity --es tiny.join "<server>|<room>"
//! --ez tiny.camera true --ez tiny.speaker true`) and, on a desktop, `TINYCLIENT_DEBUG_JOIN`
//! (`<server>|<room>[|camera][|speaker][|share]`).
//!
//! `<server>` matches a signed-in Tiny account's name or id (empty: the only one); `<room>` a
//! room's id or name. Every step is logged as `tinyclient: debug launch: ...`. Compiled only
//! with `debug_assertions`: a release build has none of it.

use super::sidebar::Mode;
use super::state::Shell;
use gpui::Context;
use protocol::{CallState, Link, MediaRequest, Network, Request};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    pub server: String,
    pub room: String,
    pub camera: bool,
    pub speaker: bool,
    /// Share the screen once in (desktop; a phone asks for consent first).
    pub share: bool,
}

impl Launch {
    /// `<server>|<room>[|camera][|speaker]`.
    pub fn parse(text: &str) -> Option<Launch> {
        let mut parts = text.split('|').map(str::trim);
        let server = parts.next()?.to_owned();
        let room = parts.next().filter(|room| !room.is_empty())?.to_owned();
        let mut launch = Launch { server, room, ..Launch::default() };
        for flag in parts {
            match flag {
                "camera" => launch.camera = true,
                "speaker" => launch.speaker = true,
                "share" => launch.share = true,
                _ => {}
            }
        }
        Some(launch)
    }
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    /// Waiting for a signed-in, connected Tiny account.
    Account,
    /// The server's page was asked for; waiting for its rooms.
    Rooms(Instant),
    /// Join was pressed; waiting for the call to be up.
    Joining(Instant),
}

static PENDING: Mutex<Option<(Launch, Stage)>> = Mutex::new(None);

/// Asks for `launch`; the shell carries it out as soon as it can.
pub fn request(launch: Launch) {
    log::info!("tinyclient: debug launch: asked {launch:?}");
    *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = Some((launch, Stage::Account));
}

/// Takes a request from `TINYCLIENT_DEBUG_JOIN`, if set.
pub fn from_env() {
    if let Some(launch) = std::env::var("TINYCLIENT_DEBUG_JOIN").ok().as_deref().and_then(Launch::parse) {
        request(launch);
    }
}

impl Shell {
    /// Called twice a second from `Shell::new`'s timer.
    pub(super) fn debug_launch_tick(&mut self, cx: &mut Context<Self>) {
        let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
        let Some((launch, stage)) = pending.as_mut() else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let tiny: Vec<_> = snapshot.accounts.iter().filter(|a| a.key.network == Network::Tiny && a.link == Link::Connected).collect();
        let wanted = launch.server.to_lowercase();
        let account = tiny
            .iter()
            .find(|a| !wanted.is_empty() && (a.name.to_lowercase().contains(&wanted) || a.key.account.to_lowercase() == wanted))
            .or_else(|| (tiny.len() == 1).then(|| &tiny[0]))
            .map(|a| a.key.clone());
        let Some(account) = account else {
            return;
        };
        match *stage {
            Stage::Account => {
                log::info!("tinyclient: debug launch: opening the Tiny server {}", account.account);
                *stage = Stage::Rooms(Instant::now());
                drop(pending);
                self.go(Mode::Network(Network::Tiny), cx);
                self.prefer_tiny(account, cx);
            }
            Stage::Rooms(since) => {
                let Some(page) = self.tiny_page_facts().filter(|page| *page.account == account) else {
                    if since.elapsed() > Duration::from_secs(5) {
                        // The page did not come up (the window was not on the Tiny page yet): ask again.
                        *stage = Stage::Account;
                    }
                    return;
                };
                let wanted = launch.room.to_lowercase();
                let Some(room) = page.community.rooms.iter().find(|r| r.id == launch.room || r.name.to_lowercase() == wanted) else {
                    log::warn!("tinyclient: debug launch: no room {:?} on this server", launch.room);
                    *pending = None;
                    return;
                };
                let room = room.id.clone();
                log::info!("tinyclient: debug launch: opening room {room} and joining its call");
                *stage = Stage::Joining(Instant::now());
                drop(pending);
                self.tiny_pick_channel(Some(room.clone()), cx);
                self.tiny_join(room, cx);
            }
            Stage::Joining(since) => {
                let joined = self.tiny_call().is_some_and(|call| call.state == CallState::Joined);
                if !joined {
                    if since.elapsed() > Duration::from_secs(30) {
                        log::warn!("tinyclient: debug launch: the call is not up after 30 s; joining again");
                        *stage = Stage::Rooms(Instant::now());
                    }
                    return;
                }
                let launch = launch.clone();
                *pending = None;
                drop(pending);
                log::info!("tinyclient: debug launch: in the call");
                if launch.speaker {
                    log::info!("tinyclient: debug launch: sound to the speaker");
                    super::call::phone::hooks::choose_route(super::call::phone::hooks::Route::Speaker);
                }
                if launch.camera {
                    log::info!("tinyclient: debug launch: turning the camera on");
                    self.call_ask(Request::Media(MediaRequest::Camera { on: true }), cx);
                }
                if launch.share {
                    log::info!("tinyclient: debug launch: sharing the screen");
                    self.call_ask(Request::Media(MediaRequest::ShareSend { on: true }), cx);
                }
            }
        }
    }

    /// The timer that drives `debug_launch_tick`.
    pub(super) fn debug_launch_watch(cx: &mut Context<Self>) {
        from_env();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            if this.update(cx, |this, cx| this.debug_launch_tick(cx)).is_err() {
                break;
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::Launch;

    #[test]
    fn a_launch_names_a_server_a_room_and_flags() {
        assert_eq!(
            Launch::parse("Tiny server|Proof|camera|speaker"),
            Some(Launch { server: "Tiny server".into(), room: "Proof".into(), camera: true, speaker: true, share: false })
        );
        assert_eq!(Launch::parse("|r1"), Some(Launch { server: String::new(), room: "r1".into(), ..Launch::default() }));
        assert_eq!(Launch::parse("server only"), None);
    }
}
