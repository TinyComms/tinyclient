//! Discord server settings overlay: Overview (name, description, icon, banner, system/AFK
//! channels, default notifications, Danger Zone delete & transfer), Events administration,
//! plus Invites and Roles tabs wired in follow-up slices.

use super::components::{button, glyph, icon_button, segment, segmented, ButtonKind};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, Entity, MouseButton, PathPromptOptions,
    Stateful, Window,
};
use protocol::{
    ConversationKind, EventPlaceKind, Key, Network, Notify, Request, Response, SpaceEdit,
    SpaceEvent, SpaceEventEdit, SpaceInfo, SpaceRights,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::gui) enum Tab {
    Overview,
    Events,
}

#[derive(Default)]
pub(in crate::gui) struct State {
    pub open: Option<Open>,
}

pub(in crate::gui) struct Open {
    pub space: Key,
    pub space_name: String,
    pub rights: SpaceRights,
    pub tab: Tab,
    pub loading: bool,
    pub busy: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
    pub info: Option<SpaceInfo>,

    // Overview form fields
    pub name: Entity<TextInput>,
    pub description: Entity<TextInput>,
    pub icon_edit: Option<String>,
    pub banner_edit: Option<String>,
    pub system_channel_id: String,
    pub afk_channel_id: String,
    pub afk_timeout: u32,
    pub default_notify: Notify,

    // Danger zone (owner only)
    pub confirm_name: Entity<TextInput>,
    pub transfer_owner_id: Entity<TextInput>,
    pub mfa_code: Entity<TextInput>,

    // Events admin form
    pub editing_event_id: Option<String>,
    pub event_name: Entity<TextInput>,
    pub event_desc: Entity<TextInput>,
    pub event_location: Entity<TextInput>,
    pub event_place_kind: EventPlaceKind,
    pub event_channel_id: String,
    pub event_hours_ahead: i64,
    pub event_duration_hours: i64,
    pub event_image: Option<String>,
}

fn field_box(input: Entity<TextInput>) -> Div {
    div()
        .px(px(8.))
        .py(px(6.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::canvas())
        .border_1()
        .border_color(theme::border())
        .type_ui()
        .child(input)
}

impl Shell {
    pub(in crate::gui) fn open_discord_settings(&mut self, space: Key, cx: &mut Context<Self>) {
        let found = self.spaces.iter().find(|s| s.key == space).cloned();
        let space_name = found.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| "Server".to_owned());
        let rights = found.map(|s| s.rights).unwrap_or_default();
        if !rights.any_settings() {
            return;
        }
        let tab = if rights.manage_space || rights.owner {
            Tab::Overview
        } else {
            Tab::Events
        };
        let initial_name = space_name.clone();
        let name = cx.new(|cx| {
            let mut input = TextInput::new("Server name", cx);
            input.set_text(&initial_name, cx);
            input
        });
        let description = cx.new(|cx| TextInput::new("Short server description", cx));
        let confirm_name = cx.new(|cx| TextInput::new("Type server name to confirm", cx));
        let transfer_owner_id = cx.new(|cx| TextInput::new("Member user ID", cx));
        let mfa_code = cx.new(|cx| TextInput::new("6-digit 2FA code (if required)", cx));
        let event_name = cx.new(|cx| TextInput::new("Event title", cx));
        let event_desc = cx.new(|cx| TextInput::new("What is this event about?", cx));
        let event_location = cx.new(|cx| TextInput::new("Location or https:// link", cx));
        self.discord_settings.open = Some(Open {
            space: space.clone(),
            space_name,
            rights,
            tab,
            loading: true,
            busy: false,
            notice: None,
            error: None,
            info: None,
            name,
            description,
            icon_edit: None,
            banner_edit: None,
            system_channel_id: String::new(),
            afk_channel_id: String::new(),
            afk_timeout: 300,
            default_notify: Notify::All,
            confirm_name,
            transfer_owner_id,
            mfa_code,
            editing_event_id: None,
            event_name,
            event_desc,
            event_location,
            event_place_kind: EventPlaceKind::Voice,
            event_channel_id: String::new(),
            event_hours_ahead: 1,
            event_duration_hours: 1,
            event_image: None,
        });
        self.load_discord_settings_info(space.clone(), cx);
        if rights.manage_events {
            self.load_space_events(space, cx);
        }
        cx.notify();
    }

    pub(in crate::gui) fn close_discord_settings(&mut self, cx: &mut Context<Self>) {
        if self.discord_settings.open.take().is_some() {
            cx.notify();
        }
    }

    fn load_discord_settings_info(&mut self, space: Key, cx: &mut Context<Self>) {
        let req = Request::SpaceInfo { space: space.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(open) = this.discord_settings.open.as_mut() else {
                    return;
                };
                if open.space != space {
                    return;
                }
                open.loading = false;
                match answer {
                    Ok(Response::SpaceInfo { info }) => {
                        let name_text = info.space.name.clone();
                        let desc_text = info.description.clone();
                        open.space_name = name_text.clone();
                        open.system_channel_id = info.system_channel_id.clone().unwrap_or_default();
                        open.afk_channel_id = info.afk_channel_id.clone().unwrap_or_default();
                        open.afk_timeout = if info.afk_timeout > 0 { info.afk_timeout } else { 300 };
                        open.default_notify = info.default_notify.unwrap_or(Notify::All);
                        let name_entity = open.name.clone();
                        let desc_entity = open.description.clone();
                        open.info = Some(*info);
                        name_entity.update(cx, |i, cx| i.set_text(&name_text, cx));
                        desc_entity.update(cx, |i, cx| i.set_text(&desc_text, cx));
                    }
                    Ok(Response::Error { detail, .. }) => open.error = Some(detail),
                    Err(err) => open.error = Some(err.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_discord_settings_image(&mut self, for_banner: bool, for_event: bool, cx: &mut Context<Self>) {
        let prompt = if for_event {
            "Choose event cover image"
        } else if for_banner {
            "Choose server banner"
        } else {
            "Choose server icon"
        };
        let asked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn(async move |shell, cx| {
            if let Ok(Ok(Some(paths))) = asked.await {
                if let Some(path) = paths.into_iter().next() {
                    let chosen = path.to_string_lossy().into_owned();
                    let _ = shell.update(cx, |this, cx| {
                        if let Some(open) = this.discord_settings.open.as_mut() {
                            if for_event {
                                open.event_image = Some(chosen);
                            } else if for_banner {
                                open.banner_edit = Some(chosen);
                            } else {
                                open.icon_edit = Some(chosen);
                            }
                            open.error = None;
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    fn save_discord_overview(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.discord_settings.open.as_mut() else {
            return;
        };
        if open.busy {
            return;
        }
        let name = open.name.read(cx).text().trim().to_owned();
        if !(2..=100).contains(&name.chars().count()) {
            open.error = Some("Server name must be between 2 and 100 characters.".to_owned());
            cx.notify();
            return;
        }
        let description = open.description.read(cx).text().trim().to_owned();
        let edit = SpaceEdit {
            name: Some(name),
            description: Some(description),
            icon: open.icon_edit.clone(),
            banner: open.banner_edit.clone(),
            system_channel_id: Some(open.system_channel_id.clone()),
            afk_channel_id: Some(open.afk_channel_id.clone()),
            afk_timeout: Some(open.afk_timeout),
            default_notify: Some(open.default_notify),
        };
        let space = open.space.clone();
        open.busy = true;
        open.error = None;
        open.notice = None;
        cx.notify();
        let req = Request::EditSpace { space: space.clone(), edit };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(open) = this.discord_settings.open.as_mut() {
                    open.busy = false;
                    match answer {
                        Ok(Response::Ok) => {
                            open.icon_edit = None;
                            open.banner_edit = None;
                            open.notice = Some("Server settings saved.".to_owned());
                            open.error = None;
                            this.load_discord_settings_info(space, cx);
                        }
                        Ok(Response::Error { detail, .. }) => open.error = Some(detail),
                        Err(err) => open.error = Some(err.to_string()),
                        _ => open.error = Some("Could not save settings.".to_owned()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_discord_delete(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.discord_settings.open.as_mut() else {
            return;
        };
        if open.busy {
            return;
        }
        let typed = open.confirm_name.read(cx).text().trim().to_owned();
        if typed != open.space_name {
            open.error = Some(format!("Type \"{}\" exactly to confirm deletion.", open.space_name));
            cx.notify();
            return;
        }
        let mfa = open.mfa_code.read(cx).text().trim().to_owned();
        let mfa_code = (!mfa.is_empty()).then_some(mfa);
        let space = open.space.clone();
        open.busy = true;
        open.error = None;
        open.notice = None;
        cx.notify();
        let req = Request::DeleteSpace { space, mfa_code };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.discord_settings.open = None;
                    this.go(super::sidebar::Mode::Network(Network::Discord), cx);
                    this.toast(Some(("icons/trash-2.svg", theme::text_soft())), "Server deleted", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(open) = this.discord_settings.open.as_mut() {
                        open.busy = false;
                        open.error = Some(detail);
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(open) = this.discord_settings.open.as_mut() {
                        open.busy = false;
                        open.error = Some(err.to_string());
                    }
                    cx.notify();
                }
                _ => {
                    if let Some(open) = this.discord_settings.open.as_mut() {
                        open.busy = false;
                        open.error = Some("Could not delete server.".to_owned());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn submit_discord_transfer(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.discord_settings.open.as_mut() else {
            return;
        };
        if open.busy {
            return;
        }
        let typed = open.confirm_name.read(cx).text().trim().to_owned();
        if typed != open.space_name {
            open.error = Some(format!("Type \"{}\" exactly to confirm ownership transfer.", open.space_name));
            cx.notify();
            return;
        }
        let new_id = open.transfer_owner_id.read(cx).text().trim().to_owned();
        if new_id.is_empty() {
            open.error = Some("Enter the member's user ID to transfer ownership.".to_owned());
            cx.notify();
            return;
        }
        let mfa = open.mfa_code.read(cx).text().trim().to_owned();
        let mfa_code = (!mfa.is_empty()).then_some(mfa);
        let space = open.space.clone();
        let new_owner = Key::new(&space.account(), new_id);
        open.busy = true;
        open.error = None;
        open.notice = None;
        cx.notify();
        let req = Request::TransferSpace { space, new_owner, mfa_code };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(open) = this.discord_settings.open.as_mut() {
                    open.busy = false;
                    match answer {
                        Ok(Response::Ok) => {
                            open.rights.owner = false;
                            open.notice = Some("Ownership transferred.".to_owned());
                            open.error = None;
                        }
                        Ok(Response::Error { detail, .. }) => open.error = Some(detail),
                        Err(err) => open.error = Some(err.to_string()),
                        _ => open.error = Some("Could not transfer ownership.".to_owned()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_discord_event(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.discord_settings.open.as_mut() else {
            return;
        };
        if open.busy {
            return;
        }
        let name = open.event_name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            open.error = Some("Enter an event title.".to_owned());
            cx.notify();
            return;
        }
        let description = open.event_desc.read(cx).text().trim().to_owned();
        let location = open.event_location.read(cx).text().trim().to_owned();
        let start = crate::view::now() + open.event_hours_ahead.max(1) * 3600;
        let end = Some(start + open.event_duration_hours.max(1) * 3600);
        let edit = SpaceEventEdit {
            name,
            description,
            start,
            end,
            place_kind: open.event_place_kind,
            channel_id: (!open.event_channel_id.is_empty()).then(|| open.event_channel_id.clone()),
            location: (!location.is_empty()).then_some(location),
            image: open.event_image.clone(),
        };
        let space = open.space.clone();
        let editing_id = open.editing_event_id.clone();
        open.busy = true;
        open.error = None;
        open.notice = None;
        cx.notify();
        let req = match editing_id {
            Some(event_id) => Request::EditEvent { space: space.clone(), event_id, event: edit },
            None => Request::CreateEvent { space: space.clone(), event: edit },
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(open) = this.discord_settings.open.as_mut() {
                    open.busy = false;
                    match answer {
                        Ok(Response::Event { .. }) => {
                            open.editing_event_id = None;
                            open.event_image = None;
                            let n = open.event_name.clone();
                            let d = open.event_desc.clone();
                            let l = open.event_location.clone();
                            open.notice = Some("Event saved.".to_owned());
                            open.error = None;
                            n.update(cx, |i, cx| i.clear(cx));
                            d.update(cx, |i, cx| i.clear(cx));
                            l.update(cx, |i, cx| i.clear(cx));
                            this.load_space_events(space, cx);
                        }
                        Ok(Response::Error { detail, .. }) => open.error = Some(detail),
                        Err(err) => open.error = Some(err.to_string()),
                        _ => open.error = Some("Could not save event.".to_owned()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_discord_event(&mut self, space: Key, event_id: String, cx: &mut Context<Self>) {
        let Some(open) = self.discord_settings.open.as_mut() else {
            return;
        };
        if open.busy {
            return;
        }
        open.busy = true;
        open.error = None;
        cx.notify();
        let req = Request::DeleteEvent { space: space.clone(), event_id };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(open) = this.discord_settings.open.as_mut() {
                    open.busy = false;
                    match answer {
                        Ok(Response::Ok) => {
                            open.notice = Some("Event canceled.".to_owned());
                            this.load_space_events(space, cx);
                        }
                        Ok(Response::Error { detail, .. }) => open.error = Some(detail),
                        Err(err) => open.error = Some(err.to_string()),
                        _ => {}
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_edit_discord_event(&mut self, ev: &SpaceEvent, cx: &mut Context<Self>) {
        let Some(open) = self.discord_settings.open.as_mut() else {
            return;
        };
        open.editing_event_id = Some(ev.key.id.clone());
        open.event_place_kind = ev.place_kind;
        open.event_channel_id = ev.conversation.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let n = open.event_name.clone();
        let d = open.event_desc.clone();
        let l = open.event_location.clone();
        let name = ev.name.clone();
        let desc = ev.description.clone();
        let place = if ev.place_kind == EventPlaceKind::External { ev.place.clone() } else { String::new() };
        n.update(cx, |i, cx| i.set_text(&name, cx));
        d.update(cx, |i, cx| i.set_text(&desc, cx));
        l.update(cx, |i, cx| i.set_text(&place, cx));
        cx.notify();
    }

    pub(in crate::gui) fn discord_settings_layer(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let open = self.discord_settings.open.as_ref()?;
        let mut tabs = div().w(px(176.)).flex_none().border_r_1().border_color(theme::border()).p(px(12.)).flex().flex_col().gap(px(4.));
        tabs = tabs.child(
            div()
                .px(px(8.))
                .pb(px(8.))
                .min_w_0()
                .truncate()
                .type_ui()
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::text())
                .child(open.space_name.clone()),
        );
        let tab_btn = |id: &'static str, icon: &'static str, label: &'static str, target: Tab, active: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .h(px(30.))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .cursor_pointer()
                .when(active, |d| d.bg(theme::selected()))
                .hover(|s| s.bg(theme::hover()))
                .flex()
                .items_center()
                .gap(px(8.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(open) = this.discord_settings.open.as_mut() {
                        open.tab = target;
                        open.error = None;
                        open.notice = None;
                        cx.notify();
                    }
                }))
                .child(glyph(icon, 15., if active { theme::text() } else { theme::text_soft() }))
                .child(div().type_ui().text_color(if active { theme::text() } else { theme::text_soft() }).child(label))
        };
        if open.rights.manage_space || open.rights.owner {
            tabs = tabs.child(tab_btn("ds-tab-overview", "icons/settings.svg", "Overview", Tab::Overview, open.tab == Tab::Overview, cx));
        }
        if open.rights.manage_events {
            tabs = tabs.child(tab_btn("ds-tab-events", "icons/calendar.svg", "Events", Tab::Events, open.tab == Tab::Events, cx));
        }

        let content: AnyElement = match open.tab {
            Tab::Overview => self.discord_overview_tab(open, cx).into_any_element(),
            Tab::Events => self.discord_events_tab(open, cx).into_any_element(),
        };

        let panel = div()
            .id("discord-settings-panel")
            .w(px(740.))
            .max_w_full()
            .h(px(580.))
            .max_h_full()
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .overflow_hidden()
            .flex()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(44.))
                            .px(px(16.))
                            .border_b_1()
                            .border_color(theme::border())
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().type_title().text_color(theme::text()).child(match open.tab {
                                Tab::Overview => "Server overview",
                                Tab::Events => "Scheduled events",
                            }))
                            .child(icon_button("ds-close", "icons/x.svg", false).on_click(cx.listener(|this, _, _, cx| {
                                this.close_discord_settings(cx);
                            }))),
                    )
                    .child(
                        div()
                            .id("discord-settings-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(px(16.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .children(open.error.as_ref().map(|e| div().type_small().text_color(theme::danger()).child(e.clone())))
                            .children(open.notice.as_ref().map(|n| div().type_small().text_color(theme::online()).child(n.clone())))
                            .child(content),
                    ),
            );

        Some(
            div()
                .id("discord-settings-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .p(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.close_discord_settings(cx);
                }))
                .child(panel),
        )
    }

    fn discord_overview_tab(&self, open: &Open, cx: &mut Context<Self>) -> Div {
        let tree = self.trees.get(&open.space);
        let text_channels: Vec<(String, String)> = tree
            .map(|t| {
                t.conversations
                    .iter()
                    .filter(|c| matches!(c.kind, ConversationKind::Channel | ConversationKind::Announcement))
                    .map(|c| (c.key.id.clone(), c.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let voice_channels: Vec<(String, String)> = tree
            .map(|t| {
                t.conversations
                    .iter()
                    .filter(|c| c.kind == ConversationKind::Voice)
                    .map(|c| (c.key.id.clone(), c.title.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut sys_bar = div().flex().flex_wrap().gap(px(6.));
        sys_bar = sys_bar.child(
            segment("ds-sys-none", "None", open.system_channel_id.is_empty())
                .px(px(8.))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(o) = this.discord_settings.open.as_mut() {
                        o.system_channel_id.clear();
                        cx.notify();
                    }
                })),
        );
        for (ix, (id, title)) in text_channels.iter().take(12).enumerate() {
            let selected = open.system_channel_id == *id;
            let chosen = id.clone();
            sys_bar = sys_bar.child(
                segment(("ds-sys-ch", ix), format!("#{}", title.trim_start_matches('#')), selected)
                    .px(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.system_channel_id = chosen.clone();
                            cx.notify();
                        }
                    })),
            );
        }

        let mut afk_bar = div().flex().flex_wrap().gap(px(6.));
        afk_bar = afk_bar.child(
            segment("ds-afk-none", "None", open.afk_channel_id.is_empty())
                .px(px(8.))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(o) = this.discord_settings.open.as_mut() {
                        o.afk_channel_id.clear();
                        cx.notify();
                    }
                })),
        );
        for (ix, (id, title)) in voice_channels.iter().take(12).enumerate() {
            let selected = open.afk_channel_id == *id;
            let chosen = id.clone();
            afk_bar = afk_bar.child(
                segment(("ds-afk-ch", ix), title.clone(), selected)
                    .px(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.afk_channel_id = chosen.clone();
                            cx.notify();
                        }
                    })),
            );
        }

        let timeouts: [(u32, &str); 5] = [(60, "1 min"), (300, "5 min"), (900, "15 min"), (1800, "30 min"), (3600, "1 hour")];
        let mut timeout_bar = segmented();
        for (ix, (secs, label)) in timeouts.into_iter().enumerate() {
            timeout_bar = timeout_bar.child(
                segment(("ds-afk-t", ix), label, open.afk_timeout == secs)
                    .px(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.afk_timeout = secs;
                            cx.notify();
                        }
                    })),
            );
        }

        let notify_bar = segmented()
            .child(
                segment("ds-notify-all", "All messages", open.default_notify == Notify::All)
                    .px(px(8.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.default_notify = Notify::All;
                            cx.notify();
                        }
                    })),
            )
            .child(
                segment("ds-notify-mentions", "Only @mentions", open.default_notify != Notify::All)
                    .px(px(8.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.default_notify = Notify::Mentions;
                            cx.notify();
                        }
                    })),
            );

        let icon_note = match open.icon_edit.as_deref() {
            Some("") => "Will remove icon".to_owned(),
            Some(path) => std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Selected".to_owned()),
            None => "Unchanged".to_owned(),
        };
        let banner_note = match open.banner_edit.as_deref() {
            Some("") => "Will remove banner".to_owned(),
            Some(path) => std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Selected".to_owned()),
            None => "Unchanged".to_owned(),
        };

        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().type_small().text_color(theme::text_soft()).child("Server name"))
            .child(field_box(open.name.clone()))
            .child(div().type_small().text_color(theme::text_soft()).child("Description"))
            .child(field_box(open.description.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(button("ds-pick-icon", ButtonKind::Secondary, "Choose icon…").on_click(cx.listener(|this, _, _, cx| {
                        this.pick_discord_settings_image(false, false, cx);
                    })))
                    .child(button("ds-clear-icon", ButtonKind::Ghost, "Remove icon").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.icon_edit = Some(String::new());
                            cx.notify();
                        }
                    })))
                    .child(div().type_small().text_color(theme::text_faint()).child(icon_note)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(button("ds-pick-banner", ButtonKind::Secondary, "Choose banner…").on_click(cx.listener(|this, _, _, cx| {
                        this.pick_discord_settings_image(true, false, cx);
                    })))
                    .child(button("ds-clear-banner", ButtonKind::Ghost, "Remove banner").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.banner_edit = Some(String::new());
                            cx.notify();
                        }
                    })))
                    .child(div().type_small().text_color(theme::text_faint()).child(banner_note)),
            )
            .child(div().type_small().text_color(theme::text_soft()).child("System messages channel"))
            .child(sys_bar)
            .child(div().type_small().text_color(theme::text_soft()).child("Inactive (AFK) voice channel"))
            .child(afk_bar)
            .child(div().type_small().text_color(theme::text_soft()).child("Inactive timeout"))
            .child(timeout_bar)
            .child(div().type_small().text_color(theme::text_soft()).child("Default notification settings"))
            .child(notify_bar)
            .child(
                div()
                    .mt(px(4.))
                    .flex()
                    .justify_end()
                    .child(
                        button("ds-save-overview", ButtonKind::Primary, if open.busy { "Saving…" } else { "Save changes" })
                            .when(!open.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.save_discord_overview(cx)))),
                    ),
            );

        if open.rights.owner {
            let discord_url = format!("https://discord.com/channels/{}", open.space.id);
            col = col.child(
                div()
                    .mt(px(12.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(theme::border())
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::danger()).child("Danger zone (owner only)"))
                    .child(div().type_small().text_color(theme::text_soft()).child(format!("Type \"{}\" below to confirm deleting or transferring this server.", open.space_name)))
                    .child(field_box(open.confirm_name.clone()))
                    .child(field_box(open.mfa_code.clone()))
                    .child(div().type_small().text_color(theme::text_soft()).child("Transfer ownership to member user ID"))
                    .child(field_box(open.transfer_owner_id.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                button("ds-transfer", ButtonKind::Secondary, "Transfer ownership")
                                    .when(!open.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_discord_transfer(cx)))),
                            )
                            .child(
                                button("ds-delete", ButtonKind::Danger, "Delete server")
                                    .when(!open.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_discord_delete(cx)))),
                            )
                            .child(button("ds-open-discord", ButtonKind::Ghost, "Open in Discord").on_click(cx.listener(move |this, _, window, cx| {
                                this.open_external_url(&discord_url, window, cx);
                            }))),
                    ),
            );
        }
        col
    }

    fn discord_events_tab(&self, open: &Open, cx: &mut Context<Self>) -> Div {
        let events = self.sidebar_state.space_events.get(&open.space).cloned().unwrap_or_default();
        let voice_channels: Vec<(String, String)> = self
            .trees
            .get(&open.space)
            .map(|t| {
                t.conversations
                    .iter()
                    .filter(|c| c.kind == ConversationKind::Voice)
                    .map(|c| (c.key.id.clone(), c.title.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let place_bar = segmented()
            .child(segment("ds-ev-voice", "Voice", open.event_place_kind == EventPlaceKind::Voice).px(px(8.)).on_click(cx.listener(|this, _, _, cx| {
                if let Some(o) = this.discord_settings.open.as_mut() {
                    o.event_place_kind = EventPlaceKind::Voice;
                    cx.notify();
                }
            })))
            .child(segment("ds-ev-stage", "Stage", open.event_place_kind == EventPlaceKind::Stage).px(px(8.)).on_click(cx.listener(|this, _, _, cx| {
                if let Some(o) = this.discord_settings.open.as_mut() {
                    o.event_place_kind = EventPlaceKind::Stage;
                    cx.notify();
                }
            })))
            .child(segment("ds-ev-ext", "Somewhere else", open.event_place_kind == EventPlaceKind::External).px(px(8.)).on_click(cx.listener(|this, _, _, cx| {
                if let Some(o) = this.discord_settings.open.as_mut() {
                    o.event_place_kind = EventPlaceKind::External;
                    cx.notify();
                }
            })));

        let mut ch_bar = div().flex().flex_wrap().gap(px(6.));
        for (ix, (id, title)) in voice_channels.iter().take(10).enumerate() {
            let selected = open.event_channel_id == *id;
            let chosen = id.clone();
            ch_bar = ch_bar.child(
                segment(("ds-ev-ch", ix), title.clone(), selected)
                    .px(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.event_channel_id = chosen.clone();
                            cx.notify();
                        }
                    })),
            );
        }

        let start_choices: [(i64, &str); 4] = [(1, "In 1 hour"), (3, "In 3 hours"), (24, "Tomorrow"), (168, "Next week")];
        let mut start_bar = segmented();
        for (ix, (hrs, label)) in start_choices.into_iter().enumerate() {
            start_bar = start_bar.child(
                segment(("ds-ev-start", ix), label, open.event_hours_ahead == hrs)
                    .px(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(o) = this.discord_settings.open.as_mut() {
                            o.event_hours_ahead = hrs;
                            cx.notify();
                        }
                    })),
            );
        }

        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(
                if open.editing_event_id.is_some() { "Edit event" } else { "Create event" },
            ))
            .child(field_box(open.event_name.clone()))
            .child(field_box(open.event_desc.clone()))
            .child(place_bar)
            .when(open.event_place_kind == EventPlaceKind::External, |c| c.child(field_box(open.event_location.clone())))
            .when(open.event_place_kind != EventPlaceKind::External, |c| c.child(ch_bar))
            .child(start_bar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(button("ds-ev-cover", ButtonKind::Secondary, "Cover image…").on_click(cx.listener(|this, _, _, cx| {
                        this.pick_discord_settings_image(false, true, cx);
                    })))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .when(open.editing_event_id.is_some(), |b| {
                                b.child(button("ds-ev-cancel-edit", ButtonKind::Ghost, "Cancel edit").on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(o) = this.discord_settings.open.as_mut() {
                                        o.editing_event_id = None;
                                        cx.notify();
                                    }
                                })))
                            })
                            .child(
                                button(
                                    "ds-ev-save",
                                    ButtonKind::Primary,
                                    if open.editing_event_id.is_some() { "Update event" } else { "Create event" },
                                )
                                .when(!open.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.save_discord_event(cx)))),
                            ),
                    ),
            );

        if !events.is_empty() {
            col = col.child(div().mt(px(8.)).type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Existing events"));
            for (ix, ev) in events.into_iter().enumerate() {
                let edit_ev = ev.clone();
                let del_space = open.space.clone();
                let del_id = ev.key.id.clone();
                col = col.child(
                    div()
                        .p(px(10.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::canvas())
                        .border_1()
                        .border_color(theme::border())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().type_ui().text_color(theme::text()).child(ev.name))
                                .child(div().truncate().type_small().text_color(theme::text_soft()).child(ev.place)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .child(button(("ds-ev-edit", ix), ButtonKind::Secondary, "Edit").h(px(26.)).px(px(8.)).type_small().on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_edit_discord_event(&edit_ev, cx);
                                })))
                                .child(button(("ds-ev-del", ix), ButtonKind::Danger, "Cancel").h(px(26.)).px(px(8.)).type_small().on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_discord_event(del_space.clone(), del_id.clone(), cx);
                                }))),
                        ),
                );
            }
        }
        col
    }
}
