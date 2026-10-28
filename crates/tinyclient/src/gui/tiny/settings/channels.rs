//! Channels: still to be drawn. What it holds: Categories and channels as one tree to drag; per channel its name, topic, user limit, bitrate (16 to 128 kb/s), video, push to talk required, and permission overrides per role. `Overlay::channel` is the channel it was opened from, to select first. Requests: TinyRoomUpdate, TinyRoomCreateWith, TinyRoomDelete, TinyCategoryCreate, TinyCategoryRename, TinyCategoryDelete, TinyLayout, TinyOverrideSet, TinyOverrideClear.
//!
//! The frame (`settings/mod.rs`) lists it where `Page::Channels.visible` says, and calls the
//! function below with the page as the server last said it. Its draft is [`Draft`], in
//! `Overlay::channels`; when it edits, give it `dirty`, `save` and `reset` and add them to the
//! frame's `tiny_settings_dirty`, `tiny_settings_save` and `tiny_settings_reset`.

use super::{coming, Page};
use crate::gui::state::Shell;
use crate::gui::tiny::Page as Facts;
use gpui::{AnyElement, Context, Window};

/// What has changed on the page and not been saved.
#[derive(Default)]
pub(in crate::gui) struct Draft {}

pub(in crate::gui::tiny) fn channels(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (shell, facts, window, cx);
    coming(Page::Channels, "Voice channels, their categories and their order.")
}
