//! Discord server management modal: webhooks, followed channels, bots and integrations (#407).
//!
//! Webhook URLs carry secret tokens; they are never logged and are copied only on an explicit
//! user click. External links (App Directory, per-command permissions in Discord) go through
//! `Shell::open_external_link` which enforces `http://` / `https://`.

use super::components::{button, glyph, ButtonKind};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{div, prelude::*, px, ClipboardItem, Context, Div, ElementId, Entity, MouseButton, SharedString, Stateful, Window};
use protocol::{Key, Request, Response, SpaceIntegrations, SpaceWebhook};

pub struct Modal {
    pub space: Key,
    pub space_name: String,
    pub loading: bool,
    pub error: Option<String>,
    pub integrations: Option<SpaceIntegrations>,
    /// Create-webhook form state.
    pub webhook_name: Entity<TextInput>,
    pub webhook_channel: Option<String>,
    /// Inline rename state: `(webhook_id, name_input)`.
    pub renaming_webhook: Option<(String, Entity<TextInput>)>,
}

#[derive(Default)]
pub struct State {
    pub modal: Option<Modal>,
}

impl Shell {
    /// Opens the Discord Server Settings modal on the Integrations tab.
    pub(super) fn open_discord_integrations(&mut self, space: Key, name: String, cx: &mut Context<Self>) {
        let default_channel = self
            .trees
            .get(&space)
            .and_then(|tree| {
                tree.conversations
                    .iter()
                    .find(|c| matches!(c.kind, protocol::ConversationKind::Channel | protocol::ConversationKind::Announcement))
                    .map(|c| c.key.id.clone())
            });
        let webhook_name = cx.new(|cx| TextInput::new("New webhook name", cx));
        self.discord_admin.modal = Some(Modal {
            space: space.clone(),
            space_name: name,
            loading: true,
            error: None,
            integrations: None,
            webhook_name,
            webhook_channel: default_channel,
            renaming_webhook: None,
        });
        cx.notify();
        self.reload_discord_integrations(space, cx);
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
                    Ok(Response::Error { detail, .. }) => {
                        modal.error = Some(detail);
                    }
                    Ok(_) => {
                        modal.error = Some("Unexpected answer from chatd.".to_owned());
                    }
                    Err(err) => {
                        modal.error = Some(err.to_string());
                    }
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

    fn webhook_row(&self, space: &Key, hook: &SpaceWebhook, renaming: Option<&(String, Entity<TextInput>)>, cx: &mut Context<Self>) -> Div {
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
                        col.child(input)
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

    pub(in crate::gui) fn discord_admin_modal(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.discord_admin.modal.as_ref()?;
        let space = modal.space.clone();
        let channels: Vec<(String, String)> = self
            .trees
            .get(&space)
            .map(|tree| {
                tree.conversations
                    .iter()
                    .filter(|c| matches!(c.kind, protocol::ConversationKind::Channel | protocol::ConversationKind::Announcement))
                    .map(|c| (c.key.id.clone(), c.title.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let body = if modal.loading {
            div()
                .py(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .type_small()
                .text_color(theme::text_soft())
                .child("Loading integrations…")
        } else if let Some(err) = &modal.error {
            div().py(px(16.)).type_small().text_color(theme::danger()).child(err.clone())
        } else if let Some(page) = &modal.integrations {
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
                // Channel selector chips + new webhook input.
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
                wh_section = wh_section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().child(modal.webhook_name.clone()))
                        .child(button("wh-create", ButtonKind::Primary, "New webhook").on_click(cx.listener(|this, _, _, cx| {
                            this.create_discord_webhook(cx);
                        }))),
                );
                if page.webhooks.is_empty() {
                    wh_section = wh_section.child(div().type_small().text_color(theme::text_faint()).child("No webhooks yet."));
                } else {
                    for hook in &page.webhooks {
                        wh_section = wh_section.child(self.webhook_row(&space, hook, modal.renaming_webhook.as_ref(), cx));
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
        } else {
            div()
        };

        let card = div()
            .id("discord-admin-card")
            .w(px(560.))
            .max_w_full()
            .max_h(px(540.))
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
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
                    .child(div().flex_1().type_title().text_color(theme::text()).truncate().child(format!(
                        "{} — Integrations",
                        modal.space_name
                    )))
                    .child(button("discord-admin-close", ButtonKind::Secondary, "Close").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.discord_admin.modal = None;
                            cx.notify();
                        },
                    ))),
            )
            .child(div().id("discord-admin-scroll").flex_1().min_h_0().overflow_y_scroll().child(body));

        Some(
            div()
                .id("discord-admin-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.discord_admin.modal = None;
                    cx.notify();
                }))
                .child(card),
        )
    }
}
