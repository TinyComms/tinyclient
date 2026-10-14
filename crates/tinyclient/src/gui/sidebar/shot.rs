//! `--shot` states of the lists that `gui/mod.rs` does not stage: the Telegram and Teams homes,
//! the drills, the home's Unread segment, folders and the Apps row open, the held Inbox.
//!
//! `--mode telegram|teams`, `--home-unread`, `--folder N` (a Telegram tab), `--drill
//! archive|requests|threads`, `--apps-open`, `--open-folders`, `--held N` (the "n new" pill),
//! `--friends` (People scoped to Discord), `--slack N` (the Nth workspace), `--people-search
//! letters`.

use super::home::Drill;
use super::Mode;
use crate::gui::state::Shell;
use gpui::Context;
use protocol::Network;
use std::time::Duration;

fn flag(name: &str) -> Option<String> {
    std::env::args().skip_while(|arg| arg != name).nth(1)
}

fn has(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
}

pub fn schedule(cx: &mut Context<Shell>) {
    if flag("--shot").is_none() {
        return;
    }
    cx.spawn(async move |this, cx| {
        // After `gui/mod.rs` has gone to its mode, before it opens rows.
        cx.background_executor().timer(Duration::from_millis(650)).await;
        let _ = this.update(cx, |shell, cx| shell.shot_mode(cx));
        cx.background_executor().timer(Duration::from_millis(500)).await;
        let _ = this.update(cx, |shell, cx| shell.shot_inside(cx));
    })
    .detach();
}

impl Shell {
    fn shot_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = flag("--slack").and_then(|n| n.parse::<usize>().ok()) {
            let slack: Vec<_> = self.spaces.iter().filter(|s| s.key.network == Network::Slack).map(|s| s.key.clone()).collect();
            self.sidebar_state.slack = slack.get(ix).cloned();
        }
        match flag("--mode").as_deref() {
            Some("telegram") => self.go(Mode::Network(Network::Telegram), cx),
            Some("teams") => self.go(Mode::Network(Network::Teams), cx),
            _ => self.show_mode(cx),
        }
        if has("--friends") {
            self.go(Mode::People, cx);
            self.people_scope(Some(Network::Discord), cx);
        }
    }

    fn shot_inside(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = flag("--people-search") {
            self.people_search(&text, cx);
        }
        if has("--home-unread") {
            self.set_home_unread(true, cx);
        }
        if let Some(ix) = flag("--folder").and_then(|n| n.parse::<usize>().ok()) {
            self.sidebar_state.home.folder = self.sidebar_state.home.folders.get(ix).map(|f| f.key.id.clone());
            self.load_home(cx);
        }
        if has("--open-folders") {
            let ids: Vec<String> = self.sidebar_state.home.folders.iter().map(|f| f.key.id.clone()).collect();
            self.sidebar_state.home.opened.extend(ids);
            self.sidebar_state.home.meetings_open = true;
            self.rebuild_home(cx);
        }
        if has("--apps-open") {
            self.sidebar_state.apps_open = true;
            self.rebuild_inbox(cx);
        }
        if let Some(n) = flag("--held").and_then(|n| n.parse::<usize>().ok()) {
            self.sidebar_state.pill = n;
        }
        match flag("--drill").as_deref() {
            Some("archive") => self.drill(Drill::Archive, cx),
            Some("requests") => self.drill(Drill::Requests, cx),
            Some("threads") => {
                if let Some(space) = self.slack_space() {
                    self.drill(Drill::Threads(space), cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }
}
