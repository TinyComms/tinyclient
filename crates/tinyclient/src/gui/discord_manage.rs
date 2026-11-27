//! Discord server management modal: webhooks, followed channels, bots/integrations (#407),
//! and custom emoji & stickers (#411).
//!
//! Webhook URLs carry secret tokens; they are never logged and are copied only on an explicit
//! user click. External links (App Directory, per-command permissions in Discord) go through
//! `Shell::open_external_link` which enforces `http://` / `https://`.

use super::components::{button, glyph, segment, segmented, ButtonKind, Layer};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{
    deferred, div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, ElementId, Entity, MouseButton,
    PathPromptOptions, SharedString, Window,
};
use protocol::{
    Key, Request, Response, SpaceEmoji, SpaceExpressions, SpaceIntegrations, SpaceRoleChoice,
    SpaceSticker, SpaceWebhook,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tab {
    Integrations,
    Emoji,
    Stickers,
}

pub struct Modal {
    pub space: Key,
    pub space_name: String,
    pub tab: Tab,
    pub can_integrations: bool,
    pub can_expressions: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub integrations: Option<SpaceIntegrations>,
    pub expressions: Option<SpaceExpressions>,
    /// Create-webhook form state.
    pub webhook_name: Entity<TextInput>,
    pub webhook_channel: Option<String>,
    /// Inline rename state: `(webhook_id, name_input)`.
    pub renaming_webhook: Option<(String, Entity<TextInput>)>,
    /// Create-emoji / create-sticker form inputs.
    pub emoji_name: Entity<TextInput>,
    pub sticker_name: Entity<TextInput>,
    pub sticker_tags: Entity<TextInput>,
    pub sticker_desc: Entity<TextInput>,
    /// Inline rename states for emoji and stickers.
    pub renaming_emoji: Option<(String, Entity<TextInput>)>,
    pub renaming_sticker: Option<(String, Entity<TextInput>)>,
}

#[derive(Default)]
pub struct State {
    pub modal: Option<Modal>,
}

impl Shell {
    fn open_discord_admin(&mut self, space: Key, name: String, tab: Tab, cx: &mut Context<Self>) {
        let (can_integrations, can_expressions) = self
            .spaces
            .iter()
            .find(|s| s.key == space)
            .map(|s| (s.can_manage_webhooks || s.can_manage_guild, s.can_manage_expressions))
            .unwrap_or((tab == Tab::Integrations, tab != Tab::Integrations));
        let default_channel = self.trees.get(&space).and_then(|tree| {
            tree.conversations
                .iter()
                .find(|c| {
                    matches!(
                        c.kind,
                        protocol::ConversationKind::Channel | protocol::ConversationKind::Announcement
                    )
                })
                .map(|c| c.key.id.clone())
        });
        let webhook_name = cx.new(|cx| TextInput::new("New webhook name", cx));
        let emoji_name = cx.new(|cx| TextInput::new("emoji_name", cx));
        let sticker_name = cx.new(|cx| TextInput::new("Sticker name", cx));
        let sticker_tags = cx.new(|cx| TextInput::new("Related emoji (e.g. wave)", cx));
        let sticker_desc = cx.new(|cx| TextInput::new("Description (optional)", cx));
        self.discord_admin.modal = Some(Modal {
            space: space.clone(),
            space_name: name,
            tab,
            can_integrations,
            can_expressions,
            loading: true,
            error: None,
            integrations: None,
            expressions: None,
            webhook_name,
            webhook_channel: default_channel,
            renaming_webhook: None,
            emoji_name,
            sticker_name,
            sticker_tags,
            sticker_desc,
            renaming_emoji: None,
            renaming_sticker: None,
        });
        cx.notify();
        match tab {
            Tab::Integrations => self.reload_discord_integrations(space, cx),
            Tab::Emoji | Tab::Stickers => self.reload_discord_expressions(space, cx),
        }
    }

    /// Opens the Discord Server Settings modal on the Integrations tab.
    pub(super) fn open_discord_integrations(&mut self, space: Key, name: String, cx: &mut Context<Self>) {
        self.open_discord_admin(space, name, Tab::Integrations, cx);
    }

    /// Opens the Discord Server Settings modal on the Emoji & Stickers tab.
    pub(super) fn open_discord_expressions(&mut self, space: Key, name: String, cx: &mut Context<Self>) {
        self.open_discord_admin(space, name, Tab::Emoji, cx);
    }

    fn switch_admin_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_mut() else {
            return;
        };
        if modal.tab == tab {
            return;
        }
        modal.tab = tab;
        modal.error = None;
        let space = modal.space.clone();
        let needs_integrations = tab == Tab::Integrations && modal.integrations.is_none();
        let needs_expressions = matches!(tab, Tab::Emoji | Tab::Stickers) && modal.expressions.is_none();
        if needs_integrations || needs_expressions {
            modal.loading = true;
        }
        cx.notify();
        if needs_integrations {
            self.reload_discord_integrations(space, cx);
        } else if needs_expressions {
            self.reload_discord_expressions(space, cx);
        }
    }

    fn reload_discord_integrations(&mut self, space: Key, cx: &mut Context<Self>) {
        let req = Request::SpaceIntegrations { space: space.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) else {
                    return;
                };
                modal.loading = false;
                match answer {
                    Ok(Response::SpaceIntegrations { integrations, .. }) => {
                        modal.integrations = Some(*integrations);
                        modal.error = None;
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Unexpected answer from chatd.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_discord_expressions(&mut self, space: Key, cx: &mut Context<Self>) {
        let req = Request::SpaceExpressions { space: space.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) else {
                    return;
                };
                modal.loading = false;
                match answer {
                    Ok(Response::SpaceExpressions { expressions, .. }) => {
                        modal.expressions = Some(*expressions);
                        modal.error = None;
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Unexpected answer from chatd.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_discord_webhook(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_ref() else {
            return;
        };
        let space = modal.space.clone();
        let name = modal.webhook_name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.toast(None, "Enter a name for the webhook.", cx);
            return;
        }
        let Some(channel_id) = modal.webhook_channel.clone() else {
            self.toast(None, "Pick a channel for the webhook.", cx);
            return;
        };
        let conversation = Key::new(&space.account(), channel_id);
        let req = Request::CreateWebhook { conversation, name };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceWebhook { webhook }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        modal.webhook_name.update(cx, |input, cx| input.clear(cx));
                        if let Some(page) = modal.integrations.as_mut() {
                            page.webhooks.push(*webhook);
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Webhook created", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not create webhook.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn save_webhook_rename(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_mut() else {
            return;
        };
        let space = modal.space.clone();
        let Some((webhook_id, input)) = modal.renaming_webhook.take() else {
            return;
        };
        let name = input.read(cx).text().trim().to_owned();
        if name.is_empty() {
            cx.notify();
            return;
        }
        let req = Request::EditWebhook {
            space: space.clone(),
            webhook_id: webhook_id.clone(),
            name: Some(name),
            channel_id: None,
        };
        cx.notify();
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceWebhook { webhook }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(page) = modal.integrations.as_mut() {
                            if let Some(slot) = page.webhooks.iter_mut().find(|w| w.id == webhook_id) {
                                *slot = *webhook;
                            }
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Webhook renamed", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not rename webhook.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn delete_discord_webhook(&mut self, space: Key, webhook_id: String, follower: bool, cx: &mut Context<Self>) {
        let req = Request::DeleteWebhook { space: space.clone(), webhook_id: webhook_id.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(page) = modal.integrations.as_mut() {
                            page.webhooks.retain(|w| w.id != webhook_id);
                            page.followers.retain(|w| w.id != webhook_id);
                        }
                    }
                    let msg = if follower { "Channel unfollowed" } else { "Webhook deleted" };
                    this.toast(Some(("icons/check.svg", theme::text_soft())), msg, cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not delete webhook.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn remove_discord_integration(&mut self, space: Key, integration_id: String, cx: &mut Context<Self>) {
        let req = Request::RemoveIntegration { space: space.clone(), integration_id: integration_id.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(page) = modal.integrations.as_mut() {
                            page.integrations.retain(|i| i.id != integration_id);
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Integration removed", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not remove integration.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn pick_and_upload_emoji(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_ref() else {
            return;
        };
        let space = modal.space.clone();
        let name = modal.emoji_name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.toast(None, "Enter a name for the emoji first.", cx);
            return;
        }
        let asked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Upload custom emoji".into()),
        });
        cx.spawn(async move |shell, cx| {
            let Ok(Ok(Some(paths))) = asked.await else {
                return;
            };
            let Some(first) = paths.into_iter().next() else {
                return;
            };
            let path = first.to_string_lossy().into_owned();
            let req = Request::CreateEmoji { space: space.clone(), name, path };
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceEmoji { emoji }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        modal.emoji_name.update(cx, |input, cx| input.clear(cx));
                        if let Some(expr) = modal.expressions.as_mut() {
                            expr.emojis.push(*emoji);
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Emoji uploaded", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not upload emoji.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn save_emoji_rename(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_mut() else {
            return;
        };
        let space = modal.space.clone();
        let Some((emoji_id, input)) = modal.renaming_emoji.take() else {
            return;
        };
        let name = input.read(cx).text().trim().to_owned();
        if name.is_empty() {
            cx.notify();
            return;
        }
        let req = Request::EditEmoji {
            space: space.clone(),
            emoji_id: emoji_id.clone(),
            name: Some(name),
            roles: None,
        };
        cx.notify();
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceEmoji { emoji }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(expr) = modal.expressions.as_mut() {
                            if let Some(slot) = expr.emojis.iter_mut().find(|e| e.id == emoji_id) {
                                *slot = *emoji;
                            }
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Emoji renamed", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not rename emoji.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn set_emoji_roles(&mut self, space: Key, emoji_id: String, roles: Vec<String>, cx: &mut Context<Self>) {
        let req = Request::EditEmoji {
            space: space.clone(),
            emoji_id: emoji_id.clone(),
            name: None,
            roles: Some(roles),
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceEmoji { emoji }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(expr) = modal.expressions.as_mut() {
                            if let Some(slot) = expr.emojis.iter_mut().find(|e| e.id == emoji_id) {
                                *slot = *emoji;
                            }
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Emoji roles updated", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not update emoji roles.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn delete_discord_emoji(&mut self, space: Key, emoji_id: String, cx: &mut Context<Self>) {
        let req = Request::DeleteEmoji { space: space.clone(), emoji_id: emoji_id.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(expr) = modal.expressions.as_mut() {
                            expr.emojis.retain(|e| e.id != emoji_id);
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Emoji deleted", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not delete emoji.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn pick_and_upload_sticker(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_ref() else {
            return;
        };
        let space = modal.space.clone();
        let name = modal.sticker_name.read(cx).text().trim().to_owned();
        let tags = modal.sticker_tags.read(cx).text().trim().to_owned();
        let description = modal.sticker_desc.read(cx).text().trim().to_owned();
        if name.is_empty() || tags.is_empty() {
            self.toast(None, "Enter a sticker name and related emoji tag first.", cx);
            return;
        }
        let asked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Upload custom sticker (320×320 PNG/APNG or Lottie JSON)".into()),
        });
        cx.spawn(async move |shell, cx| {
            let Ok(Ok(Some(paths))) = asked.await else {
                return;
            };
            let Some(first) = paths.into_iter().next() else {
                return;
            };
            let path = first.to_string_lossy().into_owned();
            let req = Request::CreateSticker { space: space.clone(), name, tags, description, path };
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceSticker { sticker }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        modal.sticker_name.update(cx, |input, cx| input.clear(cx));
                        modal.sticker_tags.update(cx, |input, cx| input.clear(cx));
                        modal.sticker_desc.update(cx, |input, cx| input.clear(cx));
                        if let Some(expr) = modal.expressions.as_mut() {
                            expr.stickers.push(*sticker);
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Sticker uploaded", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not upload sticker.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn save_sticker_rename(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin.modal.as_mut() else {
            return;
        };
        let space = modal.space.clone();
        let Some((sticker_id, input)) = modal.renaming_sticker.take() else {
            return;
        };
        let name = input.read(cx).text().trim().to_owned();
        if name.is_empty() {
            cx.notify();
            return;
        }
        let req = Request::EditSticker {
            space: space.clone(),
            sticker_id: sticker_id.clone(),
            name: Some(name),
            tags: None,
            description: None,
        };
        cx.notify();
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceSticker { sticker }) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(expr) = modal.expressions.as_mut() {
                            if let Some(slot) = expr.stickers.iter_mut().find(|s| s.id == sticker_id) {
                                *slot = *sticker;
                            }
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Sticker renamed", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not rename sticker.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn delete_discord_sticker(&mut self, space: Key, sticker_id: String, cx: &mut Context<Self>) {
        let req = Request::DeleteSticker { space: space.clone(), sticker_id: sticker_id.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    if let Some(modal) = this.discord_admin.modal.as_mut().filter(|m| m.space == space) {
                        if let Some(expr) = modal.expressions.as_mut() {
                            expr.stickers.retain(|s| s.id != sticker_id);
                        }
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Sticker deleted", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, "Could not delete sticker.", cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    fn webhook_row(&self, space: &Key, hook: &SpaceWebhook, renaming: Option<&(String, Entity<TextInput>)>, window: &Window, cx: &mut Context<Self>) -> Div {
        let id = hook.id.clone();
        let is_renaming = renaming.is_some_and(|(rid, _)| rid == &hook.id);
        let channel_label = if hook.channel_name.is_empty() {
            format!("#{}", hook.channel_id)
        } else {
            format!("#{}", hook.channel_name)
        };
        let meta = if hook.creator.is_empty() {
            channel_label
        } else {
            format!("{channel_label} · by {}", hook.creator)
        };
        let copy_url = hook.url.clone();
        let del_space = space.clone();
        let del_id = id.clone();
        let edit_id = id.clone();
        let edit_name = hook.name.clone();
        div()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::raised())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(is_renaming, |col| {
                        let input = renaming.map(|(_, e)| e.clone()).unwrap();
                        let focused = gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window);
                        col.child(super::components::field(focused).child(div().flex_1().min_w_0().child(input)))
                    })
                    .when(!is_renaming, |col| {
                        col.child(div().type_ui().text_color(theme::text()).truncate().child(hook.name.clone()))
                    })
                    .child(div().type_micro().text_color(theme::text_faint()).truncate().child(meta)),
            )
            .when(is_renaming, |row| {
                row.child(button(ElementId::Name(SharedString::from(format!("wh-save-{id}"))), ButtonKind::Primary, "Save").on_click(cx.listener(|this, _, _, cx| {
                    this.save_webhook_rename(cx);
                })))
            })
            .when(!is_renaming, |row| {
                row.when(!copy_url.is_empty(), |row| {
                    row.child(
                        button(ElementId::Name(SharedString::from(format!("wh-copy-{id}"))), ButtonKind::Secondary, "Copy URL").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                                this.toast(Some(("icons/check.svg", theme::text_soft())), "Webhook URL copied", cx);
                            },
                        )),
                    )
                })
                .child(
                    button(ElementId::Name(SharedString::from(format!("wh-rename-{id}"))), ButtonKind::Secondary, "Rename").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let input = cx.new(|cx| {
                                let mut field = TextInput::new("Webhook name", cx);
                                field.insert(&edit_name, cx);
                                field
                            });
                            if let Some(modal) = this.discord_admin.modal.as_mut() {
                                modal.renaming_webhook = Some((edit_id.clone(), input));
                                cx.notify();
                            }
                        },
                    )),
                )
                .child(
                    button(ElementId::Name(SharedString::from(format!("wh-del-{id}"))), ButtonKind::Danger, "Delete").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.delete_discord_webhook(del_space.clone(), del_id.clone(), false, cx);
                        },
                    )),
                )
            })
    }

    fn emoji_row(
        &self,
        space: &Key,
        emoji: &SpaceEmoji,
        roles: &[SpaceRoleChoice],
        can_manage: bool,
        renaming: Option<&(String, Entity<TextInput>)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = emoji.id.clone();
        let is_renaming = renaming.is_some_and(|(rid, _)| rid == &emoji.id);
        let edit_id = id.clone();
        let edit_name = emoji.name.clone();
        let del_space = space.clone();
        let del_id = id.clone();
        let access = if emoji.roles.is_empty() {
            "Everyone".to_owned()
        } else {
            emoji
                .roles
                .iter()
                .map(|role_id| {
                    roles
                        .iter()
                        .find(|role| role.id == *role_id)
                        .map(|role| role.name.as_str())
                        .unwrap_or(role_id.as_str())
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let meta = format!(
            "{}{} · {access}",
            if emoji.animated { "Animated" } else { "Static" },
            if emoji.creator.is_empty() { String::new() } else { format!(" · by {}", emoji.creator) }
        );
        div()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::raised())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(is_renaming, |col| {
                        let input = renaming.map(|(_, e)| e.clone()).unwrap();
                        let focused = gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window);
                        col.child(super::components::field(focused).child(div().flex_1().min_w_0().child(input)))
                    })
                    .when(!is_renaming, |col| {
                        col.child(div().type_ui().text_color(theme::text()).truncate().child(format!(":{}:", emoji.name)))
                    })
                    .child(div().type_micro().text_color(theme::text_faint()).truncate().child(meta))
                    .when(can_manage && !is_renaming && !roles.is_empty(), |col| {
                        let mut chips = div().flex().flex_wrap().gap(px(4.));
                        for role in roles {
                            let on = emoji.roles.iter().any(|role_id| role_id == &role.id);
                            let role_id = role.id.clone();
                            let emoji_id = emoji.id.clone();
                            let chip_space = space.clone();
                            let mut next = emoji.roles.clone();
                            if on {
                                next.retain(|id| id != &role_id);
                            } else {
                                next.push(role_id.clone());
                            }
                            let label = role.name.clone();
                            chips = chips.child(
                                div()
                                    .id(ElementId::Name(SharedString::from(format!("em-role-{emoji_id}-{role_id}"))))
                                    .px(px(8.))
                                    .py(px(3.))
                                    .rounded(px(theme::R_CONTROL))
                                    .bg(if on { theme::selected() } else { theme::raised() })
                                    .type_micro()
                                    .text_color(if on { theme::text() } else { theme::text_soft() })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_emoji_roles(chip_space.clone(), emoji_id.clone(), next.clone(), cx);
                                    }))
                                    .child(label),
                            );
                        }
                        col.child(chips)
                    }),
            )
            .when(can_manage && is_renaming, |row| {
                row.child(button(ElementId::Name(SharedString::from(format!("em-save-{id}"))), ButtonKind::Primary, "Save").on_click(cx.listener(|this, _, _, cx| {
                    this.save_emoji_rename(cx);
                })))
            })
            .when(can_manage && !is_renaming, |row| {
                row.child(
                    button(ElementId::Name(SharedString::from(format!("em-rename-{id}"))), ButtonKind::Secondary, "Rename").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let input = cx.new(|cx| {
                                let mut field = TextInput::new("emoji_name", cx);
                                field.insert(&edit_name, cx);
                                field
                            });
                            if let Some(modal) = this.discord_admin.modal.as_mut() {
                                modal.renaming_emoji = Some((edit_id.clone(), input));
                                cx.notify();
                            }
                        },
                    )),
                )
                .child(
                    button(ElementId::Name(SharedString::from(format!("em-del-{id}"))), ButtonKind::Danger, "Delete").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.delete_discord_emoji(del_space.clone(), del_id.clone(), cx);
                        },
                    )),
                )
            })
    }

    fn sticker_row(&self, space: &Key, sticker: &SpaceSticker, can_manage: bool, renaming: Option<&(String, Entity<TextInput>)>, window: &Window, cx: &mut Context<Self>) -> Div {
        let id = sticker.id.clone();
        let is_renaming = renaming.is_some_and(|(rid, _)| rid == &sticker.id);
        let edit_id = id.clone();
        let edit_name = sticker.name.clone();
        let del_space = space.clone();
        let del_id = id.clone();
        let fmt = match sticker.format {
            1 => "PNG",
            2 => "APNG",
            3 => "Lottie",
            4 => "GIF",
            _ => "Sticker",
        };
        let meta = if sticker.tags.is_empty() {
            fmt.to_owned()
        } else {
            format!("{fmt} · :{}", sticker.tags)
        };
        div()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::raised())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(is_renaming, |col| {
                        let input = renaming.map(|(_, e)| e.clone()).unwrap();
                        let focused = gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window);
                        col.child(super::components::field(focused).child(div().flex_1().min_w_0().child(input)))
                    })
                    .when(!is_renaming, |col| {
                        col.child(div().type_ui().text_color(theme::text()).truncate().child(sticker.name.clone()))
                    })
                    .child(div().type_micro().text_color(theme::text_faint()).truncate().child(meta)),
            )
            .when(can_manage && is_renaming, |row| {
                row.child(button(ElementId::Name(SharedString::from(format!("st-save-{id}"))), ButtonKind::Primary, "Save").on_click(cx.listener(|this, _, _, cx| {
                    this.save_sticker_rename(cx);
                })))
            })
            .when(can_manage && !is_renaming, |row| {
                row.child(
                    button(ElementId::Name(SharedString::from(format!("st-rename-{id}"))), ButtonKind::Secondary, "Rename").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let input = cx.new(|cx| {
                                let mut field = TextInput::new("Sticker name", cx);
                                field.insert(&edit_name, cx);
                                field
                            });
                            if let Some(modal) = this.discord_admin.modal.as_mut() {
                                modal.renaming_sticker = Some((edit_id.clone(), input));
                                cx.notify();
                            }
                        },
                    )),
                )
                .child(
                    button(ElementId::Name(SharedString::from(format!("st-del-{id}"))), ButtonKind::Danger, "Delete").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.delete_discord_sticker(del_space.clone(), del_id.clone(), cx);
                        },
                    )),
                )
            })
    }

    pub(in crate::gui) fn discord_admin_modal(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.discord_admin.modal.as_ref()?;
        let viewport = window.viewport_size();
        let max_w = (f32::from(viewport.width) - 32.).max(200.);
        let max_h = (f32::from(viewport.height) - 32.).max(200.);
        let space = modal.space.clone();
        let channels: Vec<(String, String)> = self
            .trees
            .get(&space)
            .map(|tree| {
                tree.conversations
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.kind,
                            protocol::ConversationKind::Channel | protocol::ConversationKind::Announcement
                        )
                    })
                    .map(|c| (c.key.id.clone(), c.title.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut tabs = segmented().flex_wrap();
        if modal.can_integrations {
            let active = modal.tab == Tab::Integrations;
            tabs = tabs.child(
                segment("admin-tab-int", "Integrations", active)
                    .on_click(cx.listener(|this, _, _, cx| this.switch_admin_tab(Tab::Integrations, cx))),
            );
        }
        if modal.can_expressions {
            let em_active = modal.tab == Tab::Emoji;
            let st_active = modal.tab == Tab::Stickers;
            tabs = tabs
                .child(
                    segment("admin-tab-emoji", "Emoji", em_active)
                        .on_click(cx.listener(|this, _, _, cx| this.switch_admin_tab(Tab::Emoji, cx))),
                )
                .child(
                    segment("admin-tab-stickers", "Stickers", st_active)
                        .on_click(cx.listener(|this, _, _, cx| this.switch_admin_tab(Tab::Stickers, cx))),
                );
        }

        let body = if modal.loading {
            div()
                .py(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .type_small()
                .text_color(theme::text_soft())
                .child("Loading…")
        } else if let Some(err) = &modal.error {
            div().py(px(16.)).type_small().text_color(theme::danger()).child(err.clone())
        } else {
            match modal.tab {
                Tab::Integrations => self.render_integrations_tab(modal, &space, &channels, window, cx),
                Tab::Emoji => self.render_emoji_tab(modal, &space, window, cx),
                Tab::Stickers => self.render_stickers_tab(modal, &space, window, cx),
            }
        };

        let card = div()
            .id("discord-admin-card")
            .w(px(580.))
            .max_w(px(max_w))
            .max_h(px(560.0_f32.min(max_h)))
            .p(px(16.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .flex()
            .flex_col()
            .gap(px(12.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph("icons/settings.svg", 16., theme::text_soft()))
                    .child(div().flex_1().min_w_0().type_title().text_color(theme::text()).truncate().child(modal.space_name.clone()))
                    .child(
                        super::components::icon_button("discord-admin-close", "icons/x.svg", false)
                            .aria_label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discord_admin.modal = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex().child(tabs))
            .child(div().id("discord-admin-scroll").flex_1().min_h_0().overflow_y_scroll().child(body));

        Some(
            deferred(
                div()
                    .id("discord-admin-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(theme::scrim())
                    .occlude()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.discord_admin.modal = None;
                        cx.notify();
                    }))
                    .child(card),
            )
            .priority(Layer::Modal as usize)
            .into_any_element(),
        )
    }

    fn render_integrations_tab(
        &self,
        modal: &Modal,
        space: &Key,
        channels: &[(String, String)],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(page) = &modal.integrations else {
            return div();
        };
        let mut content = div().flex().flex_col().gap(px(14.));
        if page.can_manage_webhooks {
            let mut wh_section = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(format!(
                    "Webhooks ({})",
                    page.webhooks.len()
                )));
            if !channels.is_empty() {
                let selected_id = modal.webhook_channel.as_deref();
                let mut picker = div().flex().flex_wrap().gap(px(4.));
                for (cid, cname) in channels.iter().take(12) {
                    let active = selected_id == Some(cid.as_str());
                    let pick_id = cid.clone();
                    picker = picker.child(
                        div()
                            .id(ElementId::Name(SharedString::from(format!("wh-ch-{cid}"))))
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(if active { theme::selected() } else { theme::raised() })
                            .type_micro()
                            .text_color(if active { theme::text() } else { theme::text_soft() })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(m) = this.discord_admin.modal.as_mut() {
                                    m.webhook_channel = Some(pick_id.clone());
                                    cx.notify();
                                }
                            }))
                            .child(format!("#{cname}")),
                    );
                }
                wh_section = wh_section.child(picker);
            }
            let wh_focused = gpui::Focusable::focus_handle(modal.webhook_name.read(cx), cx).is_focused(window);
            wh_section = wh_section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        super::components::field(wh_focused)
                            .flex_1()
                            .min_w_0()
                            .child(div().flex_1().min_w_0().child(modal.webhook_name.clone())),
                    )
                    .child(button("wh-create", ButtonKind::Primary, "New webhook").on_click(cx.listener(|this, _, _, cx| {
                        this.create_discord_webhook(cx);
                    }))),
            );
            if page.webhooks.is_empty() {
                wh_section = wh_section.child(div().type_small().text_color(theme::text_faint()).child("No webhooks yet."));
            } else {
                for hook in &page.webhooks {
                    wh_section = wh_section.child(self.webhook_row(space, hook, modal.renaming_webhook.as_ref(), window, cx));
                }
            }
            content = content.child(wh_section);

            if !page.followers.is_empty() {
                let mut fol_section = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(format!(
                        "Followed channels ({})",
                        page.followers.len()
                    )));
                for fol in &page.followers {
                    let del_space = space.clone();
                    let del_id = fol.id.clone();
                    let from = if !fol.source_guild.is_empty() && !fol.source_channel.is_empty() {
                        format!("{} › #{}", fol.source_guild, fol.source_channel)
                    } else {
                        fol.name.clone()
                    };
                    let to = if fol.channel_name.is_empty() {
                        format!("#{}", fol.channel_id)
                    } else {
                        format!("#{}", fol.channel_name)
                    };
                    fol_section = fol_section.child(
                        div()
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::raised())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(div().type_ui().text_color(theme::text()).truncate().child(from))
                                    .child(div().type_micro().text_color(theme::text_faint()).truncate().child(format!("Posts to {to}"))),
                            )
                            .child(button(ElementId::Name(SharedString::from(format!("fol-del-{}", fol.id))), ButtonKind::Danger, "Unfollow").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.delete_discord_webhook(del_space.clone(), del_id.clone(), true, cx);
                                },
                            ))),
                    );
                }
                content = content.child(fol_section);
            }
        }

        if page.can_manage_guild {
            let add_url = page.add_bot_url.clone();
            let perm_url = page.permissions_url.clone();
            let mut int_section = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(format!(
                            "Bots & Integrations ({})",
                            page.integrations.len()
                        )))
                        .when(!add_url.is_empty(), |row| {
                            row.child(button("int-add-bot", ButtonKind::Secondary, "Add app in Discord").on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_external_link(&add_url, window, cx);
                                },
                            )))
                        })
                        .when(!perm_url.is_empty(), |row| {
                            row.child(button("int-perms", ButtonKind::Secondary, "Permissions in Discord").on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_external_link(&perm_url, window, cx);
                                },
                            )))
                        }),
                );
            if page.integrations.is_empty() {
                int_section = int_section.child(div().type_small().text_color(theme::text_faint()).child("No bots or integrations installed."));
            } else {
                for item in &page.integrations {
                    let rm_space = space.clone();
                    let rm_id = item.id.clone();
                    let sub = if !item.bot_name.is_empty() {
                        format!("{} · Bot: {}", item.kind, item.bot_name)
                    } else if !item.account_name.is_empty() {
                        format!("{} · {}", item.kind, item.account_name)
                    } else {
                        item.kind.clone()
                    };
                    int_section = int_section.child(
                        div()
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::raised())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(div().type_ui().text_color(theme::text()).truncate().child(item.name.clone()))
                                    .child(div().type_micro().text_color(theme::text_faint()).truncate().child(sub)),
                            )
                            .child(button(ElementId::Name(SharedString::from(format!("int-rm-{}", item.id))), ButtonKind::Danger, "Remove").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.remove_discord_integration(rm_space.clone(), rm_id.clone(), cx);
                                },
                            ))),
                    );
                }
            }
            content = content.child(int_section);
        }
        content
    }

    fn render_emoji_tab(&self, modal: &Modal, space: &Key, window: &Window, cx: &mut Context<Self>) -> Div {
        let Some(expr) = &modal.expressions else {
            return div();
        };
        let static_count = expr.emojis.iter().filter(|e| !e.animated).count();
        let animated_count = expr.emojis.iter().filter(|e| e.animated).count();
        let mut content = div().flex().flex_col().gap(px(10.)).child(
            div().type_small().text_color(theme::text_soft()).child(format!(
                "Static: {static_count} / {} · Animated: {animated_count} / {} (Boost tier {})",
                expr.static_emoji_limit, expr.animated_emoji_limit, expr.premium_tier
            )),
        );
        if expr.can_create {
            let em_focused = gpui::Focusable::focus_handle(modal.emoji_name.read(cx), cx).is_focused(window);
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        super::components::field(em_focused)
                            .flex_1()
                            .min_w_0()
                            .child(div().flex_1().min_w_0().child(modal.emoji_name.clone())),
                    )
                    .child(button("emoji-upload", ButtonKind::Primary, "Upload emoji…").on_click(cx.listener(
                        |this, _, _, cx| this.pick_and_upload_emoji(cx),
                    ))),
            );
        }
        if expr.emojis.is_empty() {
            content = content.child(div().type_small().text_color(theme::text_faint()).child("No custom emoji yet."));
        } else {
            for emoji in &expr.emojis {
                content = content.child(self.emoji_row(space, emoji, &expr.roles, expr.can_manage, modal.renaming_emoji.as_ref(), window, cx));
            }
        }
        content
    }

    fn render_stickers_tab(&self, modal: &Modal, space: &Key, window: &Window, cx: &mut Context<Self>) -> Div {
        let Some(expr) = &modal.expressions else {
            return div();
        };
        let mut content = div().flex().flex_col().gap(px(10.)).child(
            div().type_small().text_color(theme::text_soft()).child(format!(
                "Stickers: {} / {} (Boost tier {}) · 320×320 PNG/APNG or Lottie JSON, max 512 KB",
                expr.stickers.len(),
                expr.sticker_limit,
                expr.premium_tier
            )),
        );
        if expr.can_create {
            let name_focused = gpui::Focusable::focus_handle(modal.sticker_name.read(cx), cx).is_focused(window);
            let tags_focused = gpui::Focusable::focus_handle(modal.sticker_tags.read(cx), cx).is_focused(window);
            let desc_focused = gpui::Focusable::focus_handle(modal.sticker_desc.read(cx), cx).is_focused(window);
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                super::components::field(name_focused)
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().flex_1().min_w_0().child(modal.sticker_name.clone())),
                            )
                            .child(
                                super::components::field(tags_focused)
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().flex_1().min_w_0().child(modal.sticker_tags.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                super::components::field(desc_focused)
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().flex_1().min_w_0().child(modal.sticker_desc.clone())),
                            )
                            .child(button("sticker-upload", ButtonKind::Primary, "Upload sticker…").on_click(cx.listener(
                                |this, _, _, cx| this.pick_and_upload_sticker(cx),
                            ))),
                    ),
            );
        }
        if expr.stickers.is_empty() {
            content = content.child(div().type_small().text_color(theme::text_faint()).child("No custom stickers yet."));
        } else {
            for sticker in &expr.stickers {
                content = content.child(self.sticker_row(space, sticker, expr.can_manage, modal.renaming_sticker.as_ref(), window, cx));
            }
        }
        content
    }
}
