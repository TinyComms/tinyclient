//! Bans: still to be drawn. What it holds: Banned people with the reason, who banned them, when and until when, and Unban. Requests: TinyUnban (TinyBan is the person menu's).
//!
//! The frame (`settings/mod.rs`) lists it where `Page::Bans.visible` says, and calls the
//! function below with the page as the server last said it. Its draft is [`Draft`], in
//! `Overlay::bans`; when it edits, give it `dirty`, `save` and `reset` and add them to the
//! frame's `tiny_settings_dirty`, `tiny_settings_save` and `tiny_settings_reset`.

use super::{coming, Page};
use crate::gui::state::Shell;
use crate::gui::tiny::Page as Facts;
use gpui::{AnyElement, Context, Window};

/// What has changed on the page and not been saved.
#[derive(Default)]
pub(in crate::gui) struct Draft {}

pub(in crate::gui::tiny) fn bans(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (shell, facts, window, cx);
    coming(Page::Bans, "People who can't come back, by invite, by signing in or by sign-in groups.")
}
