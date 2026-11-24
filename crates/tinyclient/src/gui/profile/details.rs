//! Details for a conversation (a group DM, a channel, a Telegram group or channel, a Teams
//! channel) and for a Space: the person pane's frame and order, with their own head.

use super::draw;
use super::facts::{self, MembersShown, NotifyControl};
use crate::gui::components::{button, button_with_icon, glyph, icon_button, pair, segment, segmented, tip, Avatar, ButtonKind, Shape as Plate};
use crate::gui::people::card::Float;
use crate::gui::people::info::{InfoPane, ManageKind, PendingMod};
use crate::gui::people::presence::{self, Ring, Shape};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Focusable, MouseButton, Window};
use protocol::{CallSupport, Conversation, ConversationKind, Key, MeetingResponse, Network, Notify, Presence, Request};

/// A Telegram channel's boost level, and a confirmed Boost for a Premium account.
fn boost_row(pane: &InfoPane, row: &Conversation, cx: &mut Context<InfoPane>) -> Option<Div> {
    if row.key.network != Network::Telegram || row.kind != ConversationKind::Channel {
        return None;
    }
    if pane.boosts_waiting(&row.key) {
        return Some(div().type_small().text_color(theme::text_faint()).child("Loading boosts…"));
    }
    let status = pane.channel_boosts(&row.key)?;
    let mut line = div().flex().flex_col().gap(px(6.)).child(div().type_ui().text_color(theme::text()).child(format!("Boosts: level {}", status.level)));
    let progress = match status.next_level_boosts {
        Some(next) => format!("{} of {next} boosts", status.boosts),
        None => format!("{} boosts", status.boosts),
    };
    line = line.child(div().type_small().text_color(theme::text_soft()).child(progress));
    if pane.boost_confirming(&row.key) {
        line = line.child(
            div()
                .p(px(8.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::raised())
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().type_small().child("Give this channel one of your boosts?"))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(button("boost-cancel", ButtonKind::Ghost, "Cancel").h(px(24.)).px(px(8.)).type_small().on_click(cx.listener(|pane, _, _, cx| pane.cancel_boost(cx))))
                        .child(button("boost-confirm", ButtonKind::Primary, "Boost").h(px(24.)).px(px(10.)).type_small().on_click(cx.listener(|pane, _, _, cx| pane.confirm_boost(cx)))),
                ),
        );
        return Some(line);
    }
    if status.my_boost {
        line = line.child(div().type_small().text_color(theme::text_soft()).child("You boosted this channel."));
    } else if status.can_boost && !status.slots.is_empty() {
        line = line.child(
            button("details-boost", ButtonKind::Secondary, "Boost")
                .h(px(26.))
                .px(px(8.))
                .type_small()
                .on_click(cx.listener(|pane, _, _, cx| pane.ask_boost_confirm(cx))),
        );
    }
    Some(line)
}

/// S6: this conversation's own notification sound, kept here only (the network never sees it),
/// over its account's and the global one. Picking one plays it.
fn sound_row(row: &Conversation, cx: &mut Context<InfoPane>) -> Div {
    use protocol::settings::{self, GLOBAL};
    let values = crate::gui::app::prefs::values();
    let scope = settings::conversation_scope(&row.key);
    let own = values.stored(&scope, settings::SOUND).map(str::to_owned);
    let inherited = values.stored(&settings::account_scope(&row.key.account()), settings::SOUND).unwrap_or_else(|| values.word(GLOBAL, settings::MESSAGE_SOUND)).to_owned();
    let names = [("pebble", "Pebble"), ("drop", "Drop"), ("tap", "Tap"), ("chime", "Chime"), ("glass", "Glass"), ("none", "None")];
    let inherited_name = names.iter().find(|(word, _)| *word == inherited).map_or("None", |(_, name)| *name);
    let choice = |ix: usize, words: String, word: Option<&'static str>, cx: &mut Context<InfoPane>| {
        let scope = scope.clone();
        let heard = word.map_or_else(|| inherited.clone(), str::to_owned);
        segment(("details-sound", ix), words, own.as_deref() == word)
            .px(px(8.))
            .on_click(cx.listener(move |_, _, _, cx| {
                let mut values = crate::gui::app::prefs::values();
                values.set(&scope, settings::SOUND, word.map(str::to_owned));
                crate::gui::app::prefs::set_values(values);
                cx.notify();
                let set = Request::Set { scope: scope.clone(), key: settings::SOUND.to_owned(), value: word.map(str::to_owned) };
                let heard = heard.clone();
                crate::gui::people::ask_later(cx, set, move |_, cx| {
                    if heard != "none" {
                        crate::gui::people::ask_later(cx, Request::PlaySound { sound: heard }, |_, _| {});
                    }
                });
            }))
    };
    let mut chips = div()
        .p(px(2.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::hover())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(2.))
        .child(choice(0, format!("Default ({inherited_name})"), None, cx));
    for (ix, (word, name)) in names.iter().enumerate() {
        chips = chips.child(choice(ix + 1, (*name).to_owned(), Some(*word), cx));
    }
    let muted = (row.muted).then(|| div().type_small().text_color(theme::text_faint()).child("Muted conversations make no sound."));
    div().flex().flex_col().gap(px(6.)).child(div().type_ui().child("Sound")).child(chips).children(muted)
}

/// Disappearing-messages timer control in the info pane (separate from local `keep_days`).
pub(in crate::gui) fn disappearing_row(
    pane: &InfoPane,
    row: &Conversation,
    info: Option<&protocol::ConversationInfo>,
    cx: &mut Context<InfoPane>,
) -> Option<Div> {
    let (_, scene, ..) = pane.parts();
    let can = scene
        .accounts
        .iter()
        .find(|a| a.key == row.key.account())
        .map(|a| a.can)
        .unwrap_or_default();
    let active = info.and_then(|i| i.disappearing).or(row.disappearing).unwrap_or(0);
    if !can.disappearing && active == 0 {
        return None;
    }
    let presets = protocol::disappearing_presets(row.key.network);
    let mut choices = div()
        .p(px(2.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::hover())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(2.));
    for (ix, &secs) in presets.iter().enumerate() {
        let key = row.key.clone();
        let label = protocol::format_disappearing(secs);
        let seg = segment(("details-disappearing", ix), label, active == secs).px(px(8.));
        let seg = if can.disappearing {
            seg.on_click(cx.listener(move |_, _, _, cx| {
                let req = Request::SetDisappearing { conversation: key.clone(), seconds: secs };
                crate::gui::people::ask_later(cx, req, |_, _| {});
            }))
        } else {
            seg
        };
        choices = choices.child(seg);
    }
    let heading = if row.key.network == Network::Telegram {
        format!("Auto-delete: {}", protocol::format_disappearing(active))
    } else {
        format!("Disappearing messages: {}", protocol::format_disappearing(active))
    };
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().type_ui().child(heading))
            .child(choices)
            .child(div().type_small().text_color(theme::text_faint()).child("Auto-deletes on the network (separate from local retention).")),
    )
}

/// Local notification level stored on this device for a Tiny conversation.
pub(in crate::gui) fn local_notify_shown(row: &Conversation) -> Notify {
    use protocol::settings::{self, GLOBAL};
    let values = crate::gui::app::prefs::values();
    let scope = settings::conversation_scope(&row.key);
    let stored = values
        .stored(&scope, settings::NOTIFY_ABOUT)
        .or_else(|| values.stored(&settings::account_scope(&row.key.account()), settings::NOTIFY_ABOUT));
    match stored {
        Some("off") => Notify::Nothing,
        Some("all") => Notify::All,
        Some(_) => Notify::Mentions,
        None if row.muted => Notify::Nothing,
        None if row.kind.is_direct() => Notify::All,
        None => match values.word(GLOBAL, settings::NOTIFY_ABOUT) {
            "all" => Notify::All,
            "off" => Notify::Nothing,
            _ => Notify::Mentions,
        },
    }
}

/// Writes `settings::NOTIFY_ABOUT` locally for a conversation without sending `Request::SetMute`.
pub(in crate::gui) fn set_local_notify(key: &Key, notify: Notify, cx: &mut gpui::App) {
    use protocol::settings;
    let word = match notify {
        Notify::All => "all",
        Notify::Mentions => "mentions",
        Notify::Nothing => "off",
    };
    let scope = settings::conversation_scope(key);
    let mut values = crate::gui::app::prefs::values();
    values.set(&scope, settings::NOTIFY_ABOUT, Some(word.to_owned()));
    crate::gui::app::prefs::set_values(values);
    let set = Request::Set {
        scope,
        key: settings::NOTIFY_ABOUT.to_owned(),
        value: Some(word.to_owned()),
    };
    crate::gui::people::ask_later(cx, set, |_, _| {});
}

/// Notifications' segmented choice for a channel.
fn notify_segments(pane: &InfoPane, row: &Conversation, local: bool, cx: &mut Context<InfoPane>) -> Div {
    let now = if local { local_notify_shown(row) } else { pane.notify_shown(row) };
    let choice = |ix: usize, words: &'static str, notify: Notify, cx: &mut Context<InfoPane>| {
        let key = row.key.clone();
        segment(("details-notify", ix), words, now == notify)
            .px(px(8.))
            .on_click(cx.listener(move |pane, _, _, cx| {
                if local {
                    set_local_notify(&key, notify, cx);
                    cx.notify();
                } else {
                    pane.notify_to(key.clone(), notify, cx);
                }
            }))
    };
    segmented()
        .child(choice(0, "All", Notify::All, cx))
        .child(choice(1, "Mentions", Notify::Mentions, cx))
        .child(choice(2, "Nothing", Notify::Nothing, cx))
}

fn head_picture(pane: &InfoPane, row: &Conversation, cx: &mut Context<InfoPane>) -> Div {
    let (hub, ..) = pane.parts();
    let size = draw::AVATAR;
    let account = row.key.account();
    let picture = hub.avatar(&account, row.avatar.as_ref(), cx);
    match row.kind {
        ConversationKind::Group if picture.is_none() => {
            // Two of its people, once they are known; the title's names until then.
            let (_, scene, .., members) = pane.parts();
            let me: Vec<&str> = scene.accounts.iter().map(|own| own.me.as_str()).collect();
            let faces: Vec<&str> = members.iter().filter(|person| !me.contains(&person.key.id.as_str())).map(|person| person.name.as_str()).take(2).collect();
            let names = if faces.len() == 2 { faces.join(", ") } else { row.title.clone() };
            pair(&row.key, &names, size, theme::sidebar())
        }
        ConversationKind::Group | ConversationKind::Direct => Avatar::new(&row.key, &row.title, size).image(picture.as_ref()).build(),
        _ if row.key.network == Network::Telegram => Avatar::new(&row.key, &row.title, size).image(picture.as_ref()).build(),
        // A channel (Teams' too) is drawn by its own mark, not a letter.
        kind => {
            let icon = match kind {
                ConversationKind::Voice => "icons/volume-2.svg",
                _ if row.private => "icons/lock.svg",
                ConversationKind::Announcement => "icons/megaphone.svg",
                _ => "icons/hash.svg",
            };
            div()
                .size(px(size))
                .rounded(px(16.))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::hairline())
                .flex()
                .items_center()
                .justify_center()
                .child(glyph(icon, 28., theme::text_soft()))
        }
    }
}

fn open_https_link(pane: &InfoPane, url: &str, window: &mut Window, cx: &mut Context<InfoPane>) {
    pane.shell_do(cx, |shell, cx| {
        let conv = shell.pane(cx);
        conv.update(cx, |conv, cx| conv.open_external_link(url, window, cx));
    });
}

/// A conversation's Details.
pub fn conversation(pane: &InfoPane, row: &Conversation, window: &Window, cx: &mut Context<InfoPane>) -> Div {
    let network = row.key.network;
    let (_, _, info, .., members) = pane.parts();
    let mut info = info.filter(|info| info.conversation.key == row.key).cloned();
    // A small group's own list, when it is the whole of it, is the count the head says.
    if let Some(info) = info.as_mut().filter(|info| matches!(facts::members_shown(row, Some(info)), MembersShown::Inline)) {
        if let Some(count) = facts::listed_count(members.len(), facts::INLINE_MEMBERS) {
            info.members = Some(count);
        }
    }
    let info = info.as_ref();
    let online = pane.online_of(network);
    let link = facts::conversation_link(row, info);
    let (waiting, failed, late) = pane.waiting();

    let mark = facts::conversation_tag(row, info).map(|words| draw::tag(words, false));
    let verified = (row.mark == Some(protocol::Mark::Verified)).then(|| glyph("icons/check.svg", 14., theme::text_soft()));
    let name = draw::name_line(row.title.clone(), None).mt(px(12.)).children(verified).children(mark);
    let (call, more) = actions(pane, row, link.clone(), online, window, cx);
    // "…" alone sits at the head's right; with Call it closes the action row.
    let (more_in_head, more) = if call.is_none() { (more, None) } else { (None, more) };
    let picture = div()
        .relative()
        .pt(px(16.))
        .child(head_picture(pane, row, cx))
        .children(more_in_head.map(|more| div().absolute().top(px(16. + draw::AVATAR - 28.)).right_0().child(more)));
    let mut top = div()
        .px(px(theme::PANE_PAD))
        .flex()
        .flex_col()
        .child(picture)
        .child(name)
        .children((!facts::conversation_line(row, info).is_empty()).then(|| {
            div().type_ui().text_color(theme::text_soft()).line_clamp(2).child(facts::conversation_line(row, info))
        }));
    if online && crate::gui::sidebar::discord_voice_editor(row) {
        let target = row.key.clone();
        top = top.child(
            div().mt(px(12.)).flex().justify_between().items_center()
                .child(div().type_small().text_color(theme::text_soft()).child("Voice"))
                .child(button_with_icon("details-discord-voice", ButtonKind::Secondary, "icons/settings.svg", "Voice settings")
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.shell_do(cx, |shell, cx| shell.open_discord_voice_settings(target.clone(), false, cx));
                    }))),
        );
    }
    if let Some(call) = call {
        top = top.child(div().mt(px(16.)).flex().items_center().gap(px(8.)).child(call).children(more));
    }
    let control = facts::notify_control(row);
    let local = matches!(control, NotifyControl::LocalSwitch | NotifyControl::LocalSegmented);
    let notify = if local { local_notify_shown(row) } else { pane.notify_shown(row) };
    let key = row.key.clone();
    top = top.child(
        draw::notifications_row(match control {
            NotifyControl::Switch => draw::switch("details-switch", notify != Notify::Nothing)
                .on_click(cx.listener(move |pane, _, _, cx| {
                    let to = if notify == Notify::Nothing { Notify::All } else { Notify::Nothing };
                    pane.notify_to(key.clone(), to, cx)
                }))
                .into_any_element(),
            NotifyControl::LocalSwitch => draw::switch("details-switch", notify != Notify::Nothing)
                .on_click(cx.listener(move |_, _, _, cx| {
                    let to = if notify == Notify::Nothing { Notify::All } else { Notify::Nothing };
                    set_local_notify(&key, to, cx);
                    cx.notify();
                }))
                .into_any_element(),
            NotifyControl::Segmented => notify_segments(pane, row, false, cx).into_any_element(),
            NotifyControl::LocalSegmented => notify_segments(pane, row, true, cx).into_any_element(),
            NotifyControl::Held(words) => div().type_small().text_color(theme::text_soft()).child(words).into_any_element(),
        })
        .mt(px(12.)),
    );
    if local {
        top = top.child(div().type_small().text_color(theme::text_faint()).child("Kept on this device."));
    }
    top = top.child(sound_row(row, cx).mt(px(8.)));
    if let Some(boosts) = boost_row(pane, row, cx) {
        top = top.child(boosts.mt(px(8.)));
    }
    if let Some(timer_row) = disappearing_row(pane, row, info, cx) {
        top = top.child(timer_row.mt(px(8.)));
    }
    let show_manage = facts::can_rename(row)
        || facts::can_edit_topic(row)
        || facts::can_add_members(row)
        || facts::can_edit_picture(row)
        || facts::can_manage_settings(row)
        || facts::can_view_admin_log(row);
    if show_manage {
        let mut manage_bar = div().mt(px(10.)).flex().flex_wrap().items_center().gap(px(6.));
        if facts::can_rename(row) {
            let key = row.key.clone();
            // An unnamed Tiny group is titled by its people, which is not a name to edit: the
            // field starts empty there, and saving it empty clears the name.
            let tiny_group = row.key.network == Network::Tiny && row.kind == ConversationKind::Group;
            let (current, hint) = if tiny_group { (String::new(), "Group name (empty to clear)") } else { (row.title.clone(), "Conversation name") };
            manage_bar = manage_bar.child(
                button("details-rename", ButtonKind::Secondary, "Edit name")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.start_manage(key.clone(), ManageKind::Rename, &current, hint, cx);
                    })),
            );
        }
        if facts::can_edit_picture(row) {
            let key = row.key.clone();
            manage_bar = manage_bar.child(
                button("details-picture", ButtonKind::Secondary, "Change picture…")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.pick_conversation_picture(key.clone(), cx);
                    })),
            );
            if row.avatar.is_some() {
                let clear_key = row.key.clone();
                manage_bar = manage_bar.child(
                    button("details-picture-clear", ButtonKind::Ghost, "Remove picture")
                        .h(px(26.))
                        .px(px(8.))
                        .type_small()
                        .on_click(cx.listener(move |pane, _, _, cx| {
                            pane.clear_conversation_picture(clear_key.clone(), cx);
                        })),
                );
            }
        }
        if facts::can_edit_topic(row) {
            let key = row.key.clone();
            let current = row.topic.clone();
            manage_bar = manage_bar.child(
                button("details-topic", ButtonKind::Secondary, "Edit topic")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.start_manage(key.clone(), ManageKind::Topic, &current, "Channel topic", cx);
                    })),
            );
            let desc_key = row.key.clone();
            let current_desc = info.map(|i| i.description.clone()).unwrap_or_default();
            manage_bar = manage_bar.child(
                button("details-description", ButtonKind::Secondary, "Edit description")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.start_manage(desc_key.clone(), ManageKind::Description, &current_desc, "Channel description", cx);
                    })),
            );
        }
        if facts::can_add_members(row) {
            let key = row.key.clone();
            manage_bar = manage_bar.child(
                button("details-add-members", ButtonKind::Secondary, "Add people…")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, window, cx| {
                        // A Tiny group picks people by name, not by id.
                        if key.network == Network::Tiny {
                            let key = key.clone();
                            let _ = pane.shell_weak().update(cx, |shell, cx| {
                                let row = shell.held_rows().find(|row| row.key == key).cloned();
                                if let Some(row) = row {
                                    shell.open_add_people(&row, window, cx);
                                }
                            });
                            return;
                        }
                        pane.start_manage(key.clone(), ManageKind::AddMembers, "", "Person IDs (comma-separated)", cx);
                    })),
            );
        }
        if facts::can_edit_picture(row) {
            let key = row.key.clone();
            manage_bar = manage_bar.child(
                button("details-picture", ButtonKind::Secondary, "Change photo")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| pane.pick_picture(key.clone(), cx))),
            );
        }
        if facts::can_manage_settings(row) {
            let protect = row.key.clone();
            let allow = row.key.clone();
            let approve = row.key.clone();
            let open = row.key.clone();
            manage_bar = manage_bar.child(
                button("details-protect", ButtonKind::Secondary, "Protect content")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.update_chat_settings(
                            protect.clone(),
                            protocol::ChatSettings { no_forwards: Some(true), ..protocol::ChatSettings::default() },
                            cx,
                        );
                    })),
            );
            manage_bar = manage_bar.child(
                button("details-forward", ButtonKind::Secondary, "Allow forwarding")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.update_chat_settings(
                            allow.clone(),
                            protocol::ChatSettings { no_forwards: Some(false), ..protocol::ChatSettings::default() },
                            cx,
                        );
                    })),
            );
            manage_bar = manage_bar.child(
                button("details-approve", ButtonKind::Secondary, "Require approval")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.update_chat_settings(
                            approve.clone(),
                            protocol::ChatSettings { join_request: Some(true), ..protocol::ChatSettings::default() },
                            cx,
                        );
                    })),
            );
            manage_bar = manage_bar.child(
                button("details-open", ButtonKind::Secondary, "Open joining")
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        pane.update_chat_settings(
                            open.clone(),
                            protocol::ChatSettings { join_request: Some(false), ..protocol::ChatSettings::default() },
                            cx,
                        );
                    })),
            );
        }
        if facts::can_view_admin_log(row) {
            let key = row.key.clone();
            let label = if pane.admin_log_for(&row.key).is_some() { "Hide actions" } else { "Recent actions" };
            manage_bar = manage_bar.child(
                button("details-admin-log", ButtonKind::Secondary, label)
                    .h(px(26.))
                    .px(px(8.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| pane.load_admin_log(key.clone(), cx))),
            );
        }
        top = top.child(manage_bar);
        if facts::can_manage_settings(row) {
            let key = row.key.clone();
            let speeds = [(0u32, "Off"), (10, "10s"), (30, "30s"), (60, "1m"), (300, "5m"), (900, "15m"), (3_600, "1h")];
            let mut chips = div().flex().flex_wrap().gap(px(4.));
            for (ix, &(secs, label)) in speeds.iter().enumerate() {
                let key = key.clone();
                chips = chips.child(segment(("details-slow", ix), label, false).on_click(cx.listener(move |pane, _, _, cx| {
                    pane.update_chat_settings(
                        key.clone(),
                        protocol::ChatSettings { slowmode_seconds: Some(secs), ..protocol::ChatSettings::default() },
                        cx,
                    );
                })));
            }
            top = top.child(
                div()
                    .mt(px(8.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .child(div().type_small().text_color(theme::text_soft()).child("Slow mode"))
                    .child(chips),
            );
        }
        if facts::can_view_admin_log(row) {
            if let Some(entries) = pane.admin_log_for(&row.key) {
                let mut list = div().mt(px(8.)).flex().flex_col().gap(px(4.));
                if entries.is_empty() {
                    list = list.child(div().type_small().text_color(theme::text_faint()).child("No recent actions."));
                } else {
                    for entry in entries.iter().take(20) {
                        let line = if entry.actor.is_empty() {
                            entry.summary.clone()
                        } else {
                            format!("{} · {}", entry.actor, entry.summary)
                        };
                        list = list.child(div().type_small().text_color(theme::text_soft()).child(line));
                    }
                }
                top = top.child(list);
            } else if pane.admin_log_loading() {
                top = top.child(div().mt(px(8.)).type_small().text_color(theme::text_faint()).child("Loading recent actions…"));
            }
        }
    }
    if network == Network::Discord
        && row.rights.manage_permissions
        && !row.kind.is_direct()
        && row.kind != ConversationKind::Thread
    {
        let perm_key = row.key.clone();
        top = top.child(
            div().mt(px(8.)).child(
                button_with_icon("details-discord-perms", ButtonKind::Secondary, "icons/lock.svg", "Permissions…")
                    .h(px(28.))
                    .px(px(10.))
                    .type_small()
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        let key = perm_key.clone();
                        pane.shell_do(cx, move |shell, cx| shell.open_channel_permissions(key, cx));
                    })),
            ),
        );
    }
    if let Some((mkey, mkind, input)) = pane.managing() {
        if mkey == &row.key {
            let discord_friends_hint = *mkind == ManageKind::AddMembers && row.key.network == Network::Discord;
            let show_history = *mkind == ManageKind::AddMembers
                && row.key.network == Network::Teams
                && row.kind == ConversationKind::Group;
            let history_label = match pane.share_history() {
                protocol::HistoryShare::None => "History: None",
                protocol::HistoryShare::Days { days: 7 } => "History: 7 days",
                protocol::HistoryShare::Days { .. } => "History: Recent",
                protocol::HistoryShare::All => "History: All",
            };
            top = top.child(
                div()
                    .mt(px(8.))
                    .p(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::raised())
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .when(discord_friends_hint, |box_| {
                        box_.child(
                            div()
                                .type_micro()
                                .text_color(theme::text_faint())
                                .child("Only Discord friends can be added (up to 10 people including you)."),
                        )
                    })
                    .child(
                        crate::gui::components::field_on(input.focus_handle(cx).is_focused(window), theme::raised())
                            .child(div().flex_1().min_w_0().child(input.clone())),
                    )
                    .when(show_history, |box_| {
                        box_.child(
                            div().flex().items_center().justify_between().child(
                                button("manage-history", ButtonKind::Secondary, history_label)
                                    .h(px(24.))
                                    .px(px(8.))
                                    .type_small()
                                    .on_click(cx.listener(|pane, _, _, cx| pane.cycle_share_history(cx))),
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(6.))
                            .child(button("manage-cancel", ButtonKind::Ghost, "Cancel").h(px(24.)).px(px(8.)).type_small().on_click(
                                cx.listener(|pane, _, _, cx| pane.cancel_manage(cx)),
                            ))
                            .child(button("manage-save", ButtonKind::Primary, "Save").h(px(24.)).px(px(10.)).type_small().on_click(
                                cx.listener(|pane, _, _, cx| pane.submit_manage(cx)),
                            )),
                    ),
            );
        }
    }
    if let Some((rkey, _, person_name)) = pane.pending_remove() {
        if rkey == &row.key {
            let prompt = facts::remove_member_confirm_text(person_name, &row.title);
            top = top.child(
                div()
                    .mt(px(8.))
                    .p(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::raised())
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().type_small().child(prompt))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(6.))
                            .child(button("remove-cancel", ButtonKind::Ghost, "Cancel").h(px(24.)).px(px(8.)).type_small().on_click(
                                cx.listener(|pane, _, _, cx| pane.cancel_remove_member(cx)),
                            ))
                            .child(button("remove-confirm", ButtonKind::Danger, "Remove").h(px(24.)).px(px(10.)).type_small().on_click(
                                cx.listener(|pane, _, _, cx| pane.confirm_remove_member(cx)),
                            )),
                    ),
            );
        }
    }
    if let Some(action) = pane.pending_mod().filter(|action| action.conversation() == &row.key) {
        let (prompt, field, confirm, kind) = match action {
            PendingMod::Ban { person_name, .. } => (
                format!("Ban {person_name} from {}? They cannot rejoin until an admin unbans them.", row.title),
                None,
                "Ban",
                ButtonKind::Danger,
            ),
            PendingMod::Promote { person_name, rank, .. } => (
                format!("Make {person_name} an admin of {}?", row.title),
                Some(rank.clone()),
                "Promote",
                ButtonKind::Primary,
            ),
            PendingMod::TransferOwner { person_name, password, .. } => (
                format!("Transfer ownership of {} to {person_name}?", row.title),
                Some(password.clone()),
                "Transfer",
                ButtonKind::Danger,
            ),
        };
        top = top.child(
            div()
                .mt(px(8.))
                .p(px(8.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::raised())
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().type_small().child(prompt))
                .children(field.map(|input| {
                    crate::gui::components::field_on(input.focus_handle(cx).is_focused(window), theme::raised())
                        .child(div().flex_1().min_w_0().child(input))
                }))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(button("mod-cancel", ButtonKind::Ghost, "Cancel").h(px(24.)).px(px(8.)).type_small().on_click(
                            cx.listener(|pane, _, _, cx| pane.cancel_mod(cx)),
                        ))
                        .child(button("mod-confirm", kind, confirm).h(px(24.)).px(px(10.)).type_small().on_click(
                            cx.listener(|pane, _, _, cx| pane.confirm_mod(cx)),
                        )),
                ),
        );
    }
    if !online {
        let words = facts::heads_up_words(&facts::HeadsUp::Offline, &row.title, network, crate::view::now());
        top = top.child(draw::heads_up_banner(&words, None).mt(px(12.)));
    }

    let mut body = div().flex().flex_col().pb(px(theme::PANE_PAD)).child(top);
    let mut sections = div().px(px(theme::PANE_PAD)).flex().flex_col();
    let mut any = false;
    let about = info.map(|info| info.description.trim().to_owned()).filter(|about| !about.is_empty()).unwrap_or_else(|| row.topic.trim().to_owned());
    if !about.is_empty() {
        any = true;
        sections = sections.child(draw::label("About", true)).child(div().type_ui().line_clamp(6).child(about));
    }
    if network == Network::Telegram && row.kind != ConversationKind::Direct {
        if let Some(url) = link.as_ref().filter(|url| protocol::link::telegram_invite_hash(url).is_some()) {
            let first = !any;
            any = true;
            let shown = url.clone();
            let copy = url.clone();
            sections = sections.child(draw::label("Invite link", first)).child(
                div()
                    .min_h(px(theme::ROW_ONE))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(shown))
                    .child(
                        button("details-invite-copy", ButtonKind::Secondary, "Copy")
                            .h(px(26.))
                            .px(px(8.))
                            .type_small()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                            })),
                    ),
            );
        }
        if let Some(count) = info.map(|info| info.pending_requests).filter(|count| *count > 0) {
            let first = !any;
            any = true;
            sections = sections.child(
                div()
                    .h(px(theme::ROW_ONE))
                    .mt(px(if first { 0. } else { 8. }))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .type_ui()
                    .text_color(theme::text())
                    .child(glyph("icons/users.svg", 16., theme::text_soft()))
                    .child(format!("Requests ({count})")),
            );
        }
    }
    match facts::members_shown(row, info) {
        MembersShown::Inline => {
            let (hub, scene, _, _, _, members) = pane.parts();
            let me_id = scene
                .accounts
                .iter()
                .find(|a| a.key == row.key.account())
                .map(|a| a.me.as_str())
                .unwrap_or("");
            if !members.is_empty() {
                sections = sections.child(draw::label("Members", !any));
                any = true;
                let ring = Ring::on(theme::sidebar());
                let mods = facts::member_mod_actions(row);
                for (ix, person) in members.iter().enumerate() {
                    let shown = match hub.presence_of(&person.key) {
                        Presence::Unknown => person.presence,
                        known => known,
                    };
                    let word = presence::word(shown, network);
                    let avatar = hub.avatar(&person.key.account(), person.avatar.as_ref(), cx);
                    let face = Avatar::new(&person.key, &person.name, 24.).image(avatar.as_ref()).build();
                    let face = face.children(Shape::of(shown).and_then(|shape| presence::dot(shape, 24., &ring)));
                    let key = person.key.clone();
                    let is_self = !me_id.is_empty() && person.key.id == me_id;
                    let can_remove = facts::can_remove_members(row) && !is_self;
                    let can_transfer = can_remove && row.key.network == Network::Discord && person.title != "Owner";
                    let remove_conv = row.key.clone();
                    let remove_id = person.key.id.clone();
                    let remove_name = person.name.clone();
                    let owner_conv = row.key.clone();
                    let owner_id = person.key.id.clone();
                    sections = sections.child(
                        div()
                            .id(("details-member", ix))
                            .h(px(theme::ROW_MID))
                            .mx(px(-8.))
                            .px(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .hover(|row| row.bg(theme::hover()))
                            .child(face)
                            .child(div().flex_1().min_w_0().truncate().type_ui().child(person.name.clone()))
                            .children((person.title == "Owner").then(|| div().flex_none().type_small().text_color(theme::text_faint()).child("Owner")))
                            .children((!word.is_empty()).then(|| div().flex_none().type_small().text_color(theme::text_faint()).child(format!("· {word}"))))
                            .when(can_transfer, |item| {
                                item.child(
                                    button(("details-member-owner", ix), ButtonKind::Ghost, "Make owner")
                                        .h(px(22.))
                                        .px(px(6.))
                                        .type_micro()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(move |pane, _, _, cx| {
                                            pane.transfer_owner(owner_conv.clone(), owner_id.clone(), cx);
                                        })),
                                )
                            })
                            .when(can_remove, |item| {
                                item.child(
                                    icon_button(("details-member-remove", ix), "icons/x.svg", false)
                                        .tooltip(tip("Remove from group", None))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(move |pane, _, _, cx| {
                                            pane.ask_remove_member(remove_conv.clone(), remove_id.clone(), remove_name.clone(), cx);
                                        })),
                                )
                            })
                            .when(mods.ban && person.title != "Owner", |item| {
                                let conv = row.key.clone();
                                let id = person.key.id.clone();
                                let name = person.name.clone();
                                item.child(
                                    button(("details-member-ban", ix), ButtonKind::Ghost, "Ban")
                                        .h(px(22.))
                                        .px(px(6.))
                                        .type_small()
                                        .tooltip(tip("Ban from group", None))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(move |pane, _, _, cx| {
                                            pane.ask_ban_member(conv.clone(), id.clone(), name.clone(), cx);
                                        })),
                                )
                            })
                            .when(mods.promote && person.title != "Owner", |item| {
                                let conv = row.key.clone();
                                let id = person.key.id.clone();
                                let name = person.name.clone();
                                item.child(
                                    button(("details-member-admin", ix), ButtonKind::Ghost, "Admin")
                                        .h(px(22.))
                                        .px(px(6.))
                                        .type_small()
                                        .tooltip(tip("Make admin", None))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(move |pane, _, _, cx| {
                                            pane.ask_promote_admin(conv.clone(), id.clone(), name.clone(), cx);
                                        })),
                                )
                            })
                            .when(mods.transfer && person.title != "Owner", |item| {
                                let conv = row.key.clone();
                                let id = person.key.id.clone();
                                let name = person.name.clone();
                                item.child(
                                    button(("details-member-owner", ix), ButtonKind::Ghost, "Owner")
                                        .h(px(22.))
                                        .px(px(6.))
                                        .type_small()
                                        .tooltip(tip("Transfer ownership", None))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(move |pane, _, _, cx| {
                                            pane.ask_transfer_owner(conv.clone(), id.clone(), name.clone(), cx);
                                        })),
                                )
                            })
                            .on_mouse_down(MouseButton::Left, cx.listener(move |pane, event: &gpui::MouseDownEvent, _, cx| {
                                let (key, at) = (key.clone(), event.position);
                                pane.shell_do(cx, move |shell, cx| shell.open_profile(key, at, cx));
                            })),
                    );
                }
            }
        }
        MembersShown::Row { members, online } => {
            sections = sections.child(draw::label("Members", !any));
            any = true;
            let words = match online {
                Some(online) => format!("{} · {} online", facts::thousands(members), facts::thousands(online)),
                None => facts::thousands(members),
            };
            sections = sections.child(
                div()
                    .id("details-members")
                    .h(px(theme::ROW_ONE))
                    .mx(px(-8.))
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .type_ui()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme::hover()))
                    .child(glyph("icons/users.svg", 16., theme::text_soft()))
                    .child(div().flex_1().child(words))
                    .child(glyph("icons/chevron-right.svg", 16., theme::text_faint()))
                    // CONTRACT: the frame has no push yet; the member pane replaces Details.
                    .on_click(cx.listener(|pane, _, _, cx| pane.shell_do(cx, |shell, cx| shell.toggle_members(cx)))),
            );
        }
        MembersShown::None => {}
    }
    if any {
        body = body.child(draw::rule().mt(px(16.))).child(sections.pt(px(4.)));
    } else if failed {
        body = body.child(draw::rule().mt(px(16.))).child(pane.failed_of(cx));
    } else if waiting && late {
        body = body.child(draw::rule().mt(px(16.))).child(div().px(px(theme::PANE_PAD)).pt(px(16.)).child(draw::skeleton()));
    }
    if let Some(bookmarks) = info.map(|i| i.bookmarks.as_slice()).filter(|b| !b.is_empty()) {
        let mut chips = div().flex().flex_wrap().gap(px(6.));
        for (ix, bm) in bookmarks.iter().enumerate() {
            let label = if bm.emoji.is_empty() {
                bm.title.clone()
            } else {
                format!("{} {}", bm.emoji, bm.title)
            };
            let url = bm.link.clone();
            let valid_url = url.starts_with("https://") || url.starts_with("http://");
            let chip = div()
                .id(("details-bookmark", ix))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::raised())
                .type_small()
                .text_color(theme::text())
                .when(valid_url, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })
                .child(label);
            chips = chips.child(chip);
        }
        body = body.child(draw::rule().mt(px(16.))).child(
            div()
                .px(px(theme::PANE_PAD))
                .pt(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child("Bookmarks"))
                .child(chips),
        );
    }
    if let Some(meeting) = info.and_then(|info| info.meeting.clone()) {
        let line = crate::gui::time::meeting_display(&meeting, crate::view::now());
        let can_join = meeting.join_url.starts_with("https://");
        let join = meeting.join_url.clone();
        let show_rsvp = meeting.response != MeetingResponse::Organizer && !meeting.id.is_empty();
        let mut actions = div().flex().flex_wrap().gap(px(6.));
        if can_join {
            actions = actions.child(button("details-meeting-join", ButtonKind::Secondary, "Join").on_click(cx.listener(move |pane, _, window, cx| {
                open_https_link(pane, &join, window, cx);
            })));
        }
        if show_rsvp {
            let account = row.key.account();
            let id = meeting.id.clone();
            let propose_at = meeting.start + 15 * 60;
            let choices = [
                ("Accept", MeetingResponse::Accepted, None),
                ("Tentative", MeetingResponse::Tentative, None),
                ("Decline", MeetingResponse::Declined, None),
                ("Propose 15 min later", MeetingResponse::Tentative, Some(propose_at)),
            ];
            for (slot, (label, response, propose)) in choices.into_iter().enumerate() {
                let account = account.clone();
                let id = id.clone();
                actions = actions.child(button(("details-meeting-rsvp", slot), ButtonKind::Ghost, label).on_click(cx.listener(move |pane, _, _, cx| {
                    pane.note_meeting_response(response);
                    crate::gui::people::ask_later(cx, Request::MeetingRsvp { account: account.clone(), id: id.clone(), response, propose }, |_, _| {});
                })));
            }
        }
        body = body.child(draw::rule().mt(px(16.))).child(
            div()
                .px(px(theme::PANE_PAD))
                .pt(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child("Meeting"))
                .child(div().type_small().text_color(theme::text_soft()).child(line))
                .child(actions),
        );
    }
    if let Some(tabs) = info.map(|i| i.tabs.as_slice()).filter(|t| !t.is_empty()) {
        let fallback_link = link.clone().unwrap_or_default();
        let mut rows = div().flex().flex_col().gap(px(2.));
        for (ix, tab) in tabs.iter().enumerate() {
            let open_url = [&tab.web_url, &tab.content_url, &fallback_link]
                .into_iter()
                .find(|u| u.starts_with("https://"))
                .cloned();
            let icon = match tab.kind.as_str() {
                "files" => "icons/folder-plus.svg",
                "planner" => "icons/check.svg",
                "website" => "icons/link.svg",
                _ => "icons/external-link.svg",
            };
            let show_app = !tab.app.is_empty() && !tab.app.eq_ignore_ascii_case(&tab.name);
            let mut row = div()
                .id(("details-tab", ix))
                .h(px(theme::ROW_ONE))
                .mx(px(-8.))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(8.))
                .type_ui()
                .child(glyph(icon, 15., theme::text_soft()))
                .child(div().flex_1().min_w_0().truncate().text_color(theme::text()).child(tab.name.clone()));
            if show_app {
                row = row.child(div().flex_none().type_micro().text_color(theme::text_faint()).child(tab.app.clone()));
            }
            if let Some(url) = open_url {
                row = row
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .child(glyph("icons/external-link.svg", 13., theme::text_faint()))
                    .on_click(cx.listener(move |pane, _, window, cx| open_https_link(pane, &url, window, cx)));
            }
            rows = rows.child(row);
        }
        body = body.child(draw::rule().mt(px(16.))).child(
            div()
                .px(px(theme::PANE_PAD))
                .pt(px(12.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child("Tabs"))
                .child(div().type_micro().text_color(theme::text_faint()).child("Tabs open in your browser or Teams."))
                .child(rows),
        );
    }
    if !waiting || late {
        body = body.child(draw::rule().mt(px(16.))).child(pane.shared_of(network, window, cx).mt(px(8.)));
    }
    let created = info.map(facts::created_line).unwrap_or_default();
    let export_key = row.key.clone();
    let export_row = div()
        .mt(px(4.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            button_with_icon("details-export", ButtonKind::Secondary, "icons/folder-plus.svg", "Export cached messages…")
                .on_click(cx.listener(move |_, _, _, cx| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    let dest = protocol::support_dir().join("exports").join(format!("conv-{now}"));
                    let req = Request::Export {
                        scope: protocol::ExportScope::Conversation { conversation: export_key.clone() },
                        destination: dest.to_string_lossy().into_owned(),
                        since: None,
                        until: None,
                        include_files: true,
                    };
                    crate::gui::people::ask_later(cx, req, |answer, cx| {
                        if let Ok(protocol::Response::Exported { path, .. }) = answer {
                            cx.open_with_system(std::path::Path::new(&path));
                        }
                    });
                })),
        )
        .child(
            div()
                .type_micro()
                .text_color(theme::text_faint())
                .child("Writes unencrypted JSON, HTML, and cached files (including messages that were end-to-end encrypted in transit). Only cached messages are included; member lists are omitted."),
        );
    body.child(
        div()
            .px(px(theme::PANE_PAD))
            .pt(px(16.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(draw::foot_line(created))
            .children(link.filter(|_| facts::foot_opens(network)).map(|url| draw::open_in("details-open", network).on_click(move |_, _, cx| cx.open_url(&url))))
            .child(export_row),
    )
}

/// Details' Call (a group DM's, a voice channel's Join) and its "…", each when it has one.
/// Channels and Telegram chats have "…" only: the design keeps calls to people and voice.
fn actions(
    pane: &InfoPane,
    row: &Conversation,
    link: Option<String>,
    online: bool,
    _: &Window,
    cx: &mut Context<InfoPane>,
) -> (Option<gpui::AnyElement>, Option<gpui::AnyElement>) {
    let network = row.key.network;
    // One place per action: the open conversation's header carries its Call, so Details does
    // not say it again for that one.
    let (_, scene, ..) = pane.parts();
    let in_header = scene.open.as_ref() == Some(&row.key);
    let calls = !in_header && network != Network::Telegram && matches!(row.kind, ConversationKind::Group | ConversationKind::Direct | ConversationKind::Voice);
    let call = match row.call {
        _ if !calls => None,
        CallSupport::Native => {
            let words = if row.kind == ConversationKind::Voice { "Join voice" } else { "Call" };
            Some(button_with_icon("details-call", ButtonKind::Secondary, "icons/phone.svg", words))
        }
        CallSupport::Handoff => {
            let icon = if network == Network::Slack { "icons/headphones.svg" } else { "icons/phone.svg" };
            Some(
                button_with_icon("details-call", ButtonKind::Secondary, icon, facts::handoff_words(network))
                    .child(glyph("icons/external-link.svg", 14., theme::text_soft())),
            )
        }
        CallSupport::None | CallSupport::Restricted => None,
    };
    let key = row.key.clone();
    let handoff = row.call == CallSupport::Handoff;
    let call = call.map(|call| {
        if online {
            call.flex_1()
                .on_click(cx.listener(move |pane, _, _, cx| {
                    let key = key.clone();
                    pane.shell_do(cx, move |shell, cx| {
                        if handoff {
                            shell.call_handoff(key, cx);
                        } else if !crate::gui::people::only_draws() {
                            shell.call_start(key, cx);
                        }
                    });
                }))
                .into_any_element()
        } else {
            call.flex_1().opacity(0.4).cursor_default().tooltip(tip(format!("{} is offline", network.label()), None)).into_any_element()
        }
    });
    let menu = facts::conversation_menu(row, link.as_deref(), false);
    let more = (!menu.is_empty()).then(|| {
        let (row, link, shell) = (row.clone(), link, pane.shell_weak());
        icon_button("details-more", "icons/ellipsis.svg", false)
            .tooltip(tip("More", None))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                cx.stop_propagation();
                let float = Float::Conversation { conversation: row.clone(), link: link.clone(), alt: window.modifiers().alt };
                let _ = shell.update(cx, |shell, cx| shell.open_float(float, event.position, cx));
            })
            .into_any_element()
    });
    (call, more)
}

/// A Space's Details: icon, name, counts, about, our nickname and roles there, Open in.
pub fn space(pane: &InfoPane, key: &Key, window: &Window, cx: &mut Context<InfoPane>) -> Div {
    let (hub, _, _, info, known, _) = pane.parts();
    let info = info.filter(|info| info.space.key == *key).cloned();
    let name = known.map(|space| space.name.clone()).or_else(|| info.as_ref().map(|info| info.space.name.clone())).unwrap_or_default();
    let icon = known.and_then(|space| space.icon.clone()).map(|icon| hub.file(&key.account(), &icon, cx));
    let banner = info
        .as_ref()
        .and_then(|info| info.banner.as_ref())
        .map(|file| hub.file(&key.account(), file, cx))
        .and_then(|file| file.path);
    let mark = (info.as_ref().and_then(|info| info.mark) == Some(protocol::Mark::Verified)).then(|| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .child(glyph("icons/badge-check.svg", 14., theme::text_soft()))
    });
    let boost_line = info
        .as_ref()
        .filter(|_| key.network == Network::Discord)
        .and_then(|info| facts::space_boost_line(info.boost_level, info.boosts));
    let (waiting, failed, late) = pane.waiting();
    let line = match (key.network, info.as_ref()) {
        (Network::Discord, Some(info)) => match (info.members, info.online) {
            (Some(members), Some(online)) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(format!("{} members ·", facts::thousands(members)))
                    .child(div().size(px(8.)).rounded(px(4.)).bg(theme::online()))
                    .child(format!("{} online", facts::thousands(online))),
            ),
            (Some(members), None) => Some(div().child(format!("{} members", facts::thousands(members)))),
            _ => None,
        },
        (Network::Teams, Some(info)) => info.members.map(|members| div().child(format!("Team · {} members", facts::thousands(members)))),
        (Network::Slack, Some(info)) if !info.domain.is_empty() => {
            Some(div().child(format!("{}.slack.com", info.domain)))
        }
        _ => None,
    };
    let top = div()
        .px(px(theme::PANE_PAD))
        .flex()
        .flex_col()
        .child(div().pt(px(16.)).child(Avatar::new(key, &name, draw::AVATAR).shape(Plate::Place).image(icon.as_ref()).build()))
        .child(draw::name_line(name.clone(), mark).mt(px(12.)))
        .children(line.map(|line| line.type_ui().text_color(theme::text_soft())))
        .children(boost_line.map(|words| div().mt(px(2.)).type_small().text_color(theme::text_faint()).child(words)));
    let top = if let Some(info) = info.as_ref().filter(|_| key.network == Network::Discord) {
        let notify = pane.space_notify_shown(key, info.notify);
        let level_seg = |ix: usize, words: &'static str, at: Notify, cx: &mut Context<InfoPane>| {
            let space_key = key.clone();
            segment(("space-notify", ix), words, notify == at)
                .px(px(8.))
                .on_click(cx.listener(move |pane, _, _, cx| {
                    let req = Request::SetSpaceOptions {
                        space: space_key.clone(),
                        notify: Some(at),
                        suppress_everyone: None,
                        suppress_roles: None,
                        highlights: None,
                        events: None,
                        mobile_push: None,
                        reset_channel: None,
                    };
                    pane.set_space_options(req, move |s| s.notify = at, cx);
                }))
        };
        let mut notify_box = div()
            .id("space-notify-row")
            .mt(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(draw::notifications_row(
                segmented()
                    .child(level_seg(0, "All", Notify::All, cx))
                    .child(level_seg(1, "Mentions", Notify::Mentions, cx))
                    .child(level_seg(2, "Nothing", Notify::Nothing, cx)),
            ));
        let toggle_row = |id: &'static str, label: &'static str, caption: Option<&'static str>, click: gpui::Stateful<Div>| {
            div()
                .id(id)
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
                        .child(div().type_small().text_color(theme::text()).child(label))
                        .children(caption.map(|c| div().type_micro().text_color(theme::text_faint()).child(c))),
                )
                .child(click)
        };
        let sk_ev = key.clone();
        let cur_everyone = info.suppress_everyone;
        notify_box = notify_box.child(toggle_row(
            "space-opt-everyone-row",
            "Suppress @everyone and @here",
            None,
            draw::switch("space-opt-everyone", cur_everyone).on_click(cx.listener(move |pane, _, _, cx| {
                let next = !cur_everyone;
                let req = Request::SetSpaceOptions {
                    space: sk_ev.clone(),
                    notify: None,
                    suppress_everyone: Some(next),
                    suppress_roles: None,
                    highlights: None,
                    events: None,
                    mobile_push: None,
                    reset_channel: None,
                };
                pane.set_space_options(req, move |s| s.suppress_everyone = next, cx);
            })),
        ));
        let sk_roles = key.clone();
        let cur_roles = info.suppress_roles;
        notify_box = notify_box.child(toggle_row(
            "space-opt-roles-row",
            "Suppress all role @mentions",
            None,
            draw::switch("space-opt-roles", cur_roles).on_click(cx.listener(move |pane, _, _, cx| {
                let next = !cur_roles;
                let req = Request::SetSpaceOptions {
                    space: sk_roles.clone(),
                    notify: None,
                    suppress_everyone: None,
                    suppress_roles: Some(next),
                    highlights: None,
                    events: None,
                    mobile_push: None,
                    reset_channel: None,
                };
                pane.set_space_options(req, move |s| s.suppress_roles = next, cx);
            })),
        ));
        let sk_hl = key.clone();
        let cur_suppress_hl = !info.highlights;
        notify_box = notify_box.child(toggle_row(
            "space-opt-highlights-row",
            "Suppress highlights",
            None,
            draw::switch("space-opt-highlights", cur_suppress_hl).on_click(cx.listener(move |pane, _, _, cx| {
                let next_highlights = cur_suppress_hl;
                let req = Request::SetSpaceOptions {
                    space: sk_hl.clone(),
                    notify: None,
                    suppress_everyone: None,
                    suppress_roles: None,
                    highlights: Some(next_highlights),
                    events: None,
                    mobile_push: None,
                    reset_channel: None,
                };
                pane.set_space_options(req, move |s| s.highlights = next_highlights, cx);
            })),
        ));
        let sk_events = key.clone();
        let cur_mute_events = !info.events;
        notify_box = notify_box.child(toggle_row(
            "space-opt-events-row",
            "Mute new events",
            None,
            draw::switch("space-opt-events", cur_mute_events).on_click(cx.listener(move |pane, _, _, cx| {
                let next_events = cur_mute_events;
                let req = Request::SetSpaceOptions {
                    space: sk_events.clone(),
                    notify: None,
                    suppress_everyone: None,
                    suppress_roles: None,
                    highlights: None,
                    events: Some(next_events),
                    mobile_push: None,
                    reset_channel: None,
                };
                pane.set_space_options(req, move |s| s.events = next_events, cx);
            })),
        ));
        let sk_push = key.clone();
        let cur_push = info.mobile_push;
        notify_box = notify_box.child(toggle_row(
            "space-opt-push-row",
            "Mobile push notifications",
            Some("Only affects Discord's own phone app."),
            draw::switch("space-opt-push", cur_push).on_click(cx.listener(move |pane, _, _, cx| {
                let next = !cur_push;
                let req = Request::SetSpaceOptions {
                    space: sk_push.clone(),
                    notify: None,
                    suppress_everyone: None,
                    suppress_roles: None,
                    highlights: None,
                    events: None,
                    mobile_push: Some(next),
                    reset_channel: None,
                };
                pane.set_space_options(req, move |s| s.mobile_push = next, cx);
            })),
        ));
        if !info.channel_overrides.is_empty() {
            let mut list = div()
                .mt(px(4.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().type_micro().font_weight(theme::MEDIUM).text_color(theme::text_faint()).child("CHANNEL OVERRIDES"));
            for (ix, over) in info.channel_overrides.iter().enumerate() {
                let level_word = match over.notify {
                    Notify::All => "All",
                    Notify::Mentions => "Mentions",
                    Notify::Nothing => "Nothing",
                };
                let sk_reset = key.clone();
                let ch_id = over.channel_id.clone();
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .type_small()
                                .text_color(theme::text_soft())
                                .child(format!("#{} · {level_word}", over.name)),
                        )
                        .child(
                            button(("space-override-reset", ix), ButtonKind::Ghost, "Reset")
                                .h(px(22.))
                                .px(px(6.))
                                .type_micro()
                                .on_click(cx.listener(move |pane, _, _, cx| {
                                    let ch = ch_id.clone();
                                    let req = Request::SetSpaceOptions {
                                        space: sk_reset.clone(),
                                        notify: None,
                                        suppress_everyone: None,
                                        suppress_roles: None,
                                        highlights: None,
                                        events: None,
                                        mobile_push: None,
                                        reset_channel: Some(ch.clone()),
                                    };
                                    pane.set_space_options(
                                        req,
                                        move |s| s.channel_overrides.retain(|o| o.channel_id != ch),
                                        cx,
                                    );
                                })),
                        ),
                );
            }
            notify_box = notify_box.child(list);
        }
        if let Some(priv_opts) = info.privacy {
            let sk_dms = key.clone();
            let cur_dms = priv_opts.allow_dms;
            let cur_reqs = priv_opts.allow_requests;
            let sk_reqs = key.clone();
            notify_box = notify_box
                .child(div().mt(px(6.)).type_micro().font_weight(theme::MEDIUM).text_color(theme::text_faint()).child("PRIVACY"))
                .child(toggle_row(
                    "space-priv-dms-row",
                    "Allow direct messages from members",
                    None,
                    draw::switch("space-priv-dms", cur_dms).on_click(cx.listener(move |pane, _, _, cx| {
                        let next = protocol::SpacePrivacy { allow_dms: !cur_dms, allow_requests: cur_reqs };
                        let req = Request::SetSpacePrivacy { space: sk_dms.clone(), privacy: next };
                        pane.set_space_options(req, move |s| s.privacy = Some(next), cx);
                    })),
                ))
                .child(toggle_row(
                    "space-priv-reqs-row",
                    "Allow message requests from members",
                    None,
                    draw::switch("space-priv-reqs", cur_reqs).on_click(cx.listener(move |pane, _, _, cx| {
                        let next = protocol::SpacePrivacy { allow_dms: cur_dms, allow_requests: !cur_reqs };
                        let req = Request::SetSpacePrivacy { space: sk_reqs.clone(), privacy: next };
                        pane.set_space_options(req, move |s| s.privacy = Some(next), cx);
                    })),
                ));
        }
        top.child(notify_box)
    } else {
        top
    };
    let mut body = div()
        .flex()
        .flex_col()
        .pb(px(theme::PANE_PAD))
        .children(banner.map(|path| {
            div()
                .w_full()
                .h(px(96.))
                .overflow_hidden()
                .bg(theme::raised())
                .child(gpui::img(std::path::PathBuf::from(path)).size_full().object_fit(gpui::ObjectFit::Cover))
        }))
        .child(top);
    let mut sections = div().px(px(theme::PANE_PAD)).flex().flex_col();
    let mut any = false;
    if let Some(info) = &info {
        if !info.description.trim().is_empty() {
            any = true;
            sections = sections.child(draw::label("About", true)).child(div().type_ui().line_clamp(6).child(info.description.trim().to_owned()));
        }
        let show_role = !info.nickname.is_empty() || !info.roles.is_empty() || info.can_change_nickname;
        if show_role {
            sections = sections.child(draw::label("Role", !any));
            any = true;
            if !info.nickname.is_empty() || !info.roles.is_empty() {
                let shown = info.roles.len().min(facts::ROLES_SHOWN);
                let role = facts::Role::Place {
                    nickname: info.nickname.clone(),
                    place: name.clone(),
                    roles: info.roles[..shown].to_vec(),
                    more: info.roles.len() - shown,
                };
                let small = Avatar::new(key, &name, 16.).shape(Plate::Place).image(icon.as_ref()).build();
                sections = sections.child(draw::role(&role, Some(small), None));
            }
            if info.can_change_nickname && pane.online_of(key.network) {
                let editing_nick = pane
                    .managing()
                    .filter(|(k, kind, _)| k == key && *kind == ManageKind::Nickname)
                    .map(|(_, _, input)| input.clone());
                if let Some(input) = editing_nick {
                    sections = sections.child(
                        div()
                            .mt(px(8.))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                crate::gui::components::field(input.focus_handle(cx).is_focused(window))
                                    .child(div().flex_1().min_w_0().child(input)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(button("space-nick-save", ButtonKind::Primary, "Save").h(px(24.)).px(px(8.)).type_small().on_click(cx.listener(|pane, _, _, cx| pane.submit_manage(cx))))
                                    .child(button("space-nick-cancel", ButtonKind::Ghost, "Cancel").h(px(24.)).px(px(8.)).type_small().on_click(cx.listener(|pane, _, _, cx| pane.cancel_manage(cx)))),
                            ),
                    );
                } else {
                    let nick_key = key.clone();
                    let current_nick = info.nickname.clone();
                    sections = sections.child(
                        div().mt(px(6.)).child(
                            button("space-change-nick", ButtonKind::Secondary, "Change nickname")
                                .h(px(26.))
                                .px(px(8.))
                                .type_small()
                                .on_click(cx.listener(move |pane, _, _, cx| {
                                    pane.start_manage(nick_key.clone(), ManageKind::Nickname, &current_nick, "Server nickname (empty to clear)", cx);
                                })),
                        ),
                    );
                }
            }
        }
        if !info.tags.is_empty() {
            sections = sections.child(draw::label("Tags", !any));
            any = true;
            let mut list = div().flex().flex_col().gap(px(6.));
            for (tix, tag) in info.tags.iter().enumerate() {
                let sub = if tag.description.is_empty() {
                    format!("{} members", tag.member_count)
                } else {
                    format!("{} members · {}", tag.member_count, tag.description)
                };
                let people: Vec<Key> = tag.members.iter().map(|m| m.key.clone()).collect();
                let chat_btn = (!people.is_empty()).then(|| {
                    let account = key.account();
                    let title = tag.name.clone();
                    let shell = pane.shell_weak();
                    button(("space-tag-chat", tix), ButtonKind::Secondary, "Chat").h(px(24.)).on_click(move |_, _, cx| {
                        let req = Request::StartGroup {
                            account: account.clone(),
                            people: people.clone(),
                            title: Some(title.clone()),
                        };
                        let shell = shell.clone();
                        crate::gui::people::ask_later(cx, req, move |answer, cx| {
                            if let Ok(protocol::Response::Conversation { conversation }) = answer {
                                let _ = shell.update(cx, |shell, cx| shell.select(conversation.key, &conversation.title, cx));
                            }
                        });
                    })
                });
                list = list.child(
                    div()
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
                                .child(div().type_ui().text_color(theme::text()).truncate().child(format!("@{}", tag.name)))
                                .child(div().type_small().text_color(theme::text_soft()).truncate().child(sub)),
                        )
                        .children(chat_btn),
                );
            }
            if key.network == Network::Teams {
                list = list.child(
                    div().pt(px(4.)).child(
                        button("space-manage-tags", ButtonKind::Ghost, "Manage tags in Teams ↗")
                            .h(px(24.))
                            .on_click(|_, _, cx| crate::gui::conversation::links::open_external_link("https://teams.microsoft.com/", cx)),
                    ),
                );
            }
            sections = sections.child(list);
        }
    }
    if any {
        body = body.child(draw::rule().mt(px(16.))).child(sections.pt(px(4.)));
    } else if failed {
        body = body.child(draw::rule().mt(px(16.))).child(pane.failed_of(cx));
    } else if waiting && late {
        body = body.child(draw::rule().mt(px(16.))).child(div().px(px(theme::PANE_PAD)).pt(px(16.)).child(draw::skeleton()));
    }
    let link = match (key.network, info.as_ref()) {
        (Network::Slack, Some(info)) if !info.url.is_empty() => Some(info.url.clone()),
        _ => info
            .as_ref()
            .map(|i| i.link.as_str())
            .filter(|u| u.starts_with("https://") && protocol::link::Destination::parse(u).is_some())
            .map(str::to_owned)
            .or_else(|| facts::space_link(key)),
    };
    let domain_or_url = info
        .as_ref()
        .map(|i| if !i.url.is_empty() { i.url.as_str() } else { i.domain.as_str() })
        .unwrap_or("");
    let slack_links = (key.network == Network::Slack).then(|| {
        let mut group = div()
            .px(px(theme::PANE_PAD))
            .pt(px(12.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(draw::label("Workspace settings · Opens in your browser", true));
        for (ix, &(label, rel)) in facts::SLACK_WORKSPACE_LINKS.iter().enumerate() {
            let url = facts::slack_workspace_url(domain_or_url, &key.id, rel);
            let shell = pane.shell_weak();
            group = group.child(
                button_with_icon(("space-slack-link", ix), ButtonKind::Secondary, "icons/external-link.svg", label)
                    .tooltip(tip(format!("{label} · Opens in your browser"), None))
                    .on_click(move |_, window, cx| {
                        let url = url.clone();
                        let _ = shell.update(cx, |shell, cx| shell.open_external_link(&url, window, cx));
                    }),
            );
        }
        group
    });
    let shell = pane.shell_weak();
    let label = if key.network == Network::Teams {
        "Manage in Teams".to_owned()
    } else {
        format!("Open in {}", key.network.label())
    };
    let network = key.network;
    body.children(slack_links).child(
        div().px(px(theme::PANE_PAD)).pt(px(16.)).children(link.map(move |url| {
            let shell = shell.clone();
            draw::open_in_labeled("space-open", network, label.clone()).on_click(move |_, window, cx| {
                if network == Network::Slack {
                    let _ = shell.update(cx, |shell, cx| shell.open_external_link(&url, window, cx));
                } else if url.starts_with("https://") {
                    let _ = shell.update(cx, |shell, cx| shell.open_external_url(&url, window, cx));
                } else {
                    cx.open_url(&url);
                }
            })
        })),
    )
}
