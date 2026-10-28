//! Roles: still to be drawn. What it holds: The roles in their order (drag to change who is above whom), each with a colour, whether it shows separately in the member list, and its permissions grouped as General, Membership, Voice, Moderation and Advanced, with a member count. Requests: TinyRoleCreate, TinyRoleUpdate, TinyRoleDelete, TinyRoleOrder.
//!
//! The frame (`settings/mod.rs`) lists it where `Page::Roles.visible` says, and calls the
//! function below with the page as the server last said it. Its draft is [`Draft`], in
//! `Overlay::roles`; when it edits, give it `dirty`, `save` and `reset` and add them to the
//! frame's `tiny_settings_dirty`, `tiny_settings_save` and `tiny_settings_reset`.

use super::{coming, Page};
use crate::gui::state::Shell;
use crate::gui::tiny::Page as Facts;
use gpui::{AnyElement, Context, Window};

/// What has changed on the page and not been saved.
#[derive(Default)]
pub(in crate::gui) struct Draft {}

pub(in crate::gui::tiny) fn roles(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (shell, facts, window, cx);
    coming(Page::Roles, "Who can do what. Roles higher in the list outrank those below them.")
}
