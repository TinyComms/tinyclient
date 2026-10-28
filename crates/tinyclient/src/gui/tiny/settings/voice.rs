//! Voice & media: still to be drawn. What it holds: The server's audio: media up or down, its public address, a check that UDP and TCP port 50000 are reachable from here, limits per room and per server, and relay status.
//!
//! The frame (`settings/mod.rs`) lists it where `Page::Voice.visible` says, and calls the
//! function below with the page as the server last said it. Its draft is [`Draft`], in
//! `Overlay::voice`; when it edits, give it `dirty`, `save` and `reset` and add them to the
//! frame's `tiny_settings_dirty`, `tiny_settings_save` and `tiny_settings_reset`.

use super::{coming, Page};
use crate::gui::state::Shell;
use crate::gui::tiny::Page as Facts;
use gpui::{AnyElement, Context, Window};

/// What has changed on the page and not been saved.
#[derive(Default)]
pub(in crate::gui) struct Draft {}

pub(in crate::gui::tiny) fn voice(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (shell, facts, window, cx);
    coming(Page::Voice, "How the server carries sound.")
}
