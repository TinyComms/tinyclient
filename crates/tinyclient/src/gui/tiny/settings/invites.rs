//! Invites: still to be drawn. What it holds: Open invites with their label, who made them, their roles, uses of the limit and when they expire, with revoke and revoke all; making one is the Invite people modal. Requests: TinyInviteWith, TinyRevoke.
//!
//! The frame (`settings/mod.rs`) lists it where `Page::Invites.visible` says, and calls the
//! function below with the page as the server last said it. Its draft is [`Draft`], in
//! `Overlay::invites`; when it edits, give it `dirty`, `save` and `reset` and add them to the
//! frame's `tiny_settings_dirty`, `tiny_settings_save` and `tiny_settings_reset`.

use super::{coming, Page};
use crate::gui::state::Shell;
use crate::gui::tiny::Page as Facts;
use gpui::{AnyElement, Context, Window};

/// What has changed on the page and not been saved.
#[derive(Default)]
pub(in crate::gui) struct Draft {}

pub(in crate::gui::tiny) fn invites(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (shell, facts, window, cx);
    coming(Page::Invites, "Codes that let people in. A code is shown once, when it is made.")
}
