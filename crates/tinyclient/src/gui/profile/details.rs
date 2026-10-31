//! Details for a conversation (a group DM, a channel, a Telegram group or channel, a Teams
//! channel) and for a Space: the person pane's frame and order, with their own head.

use super::draw;
use super::facts::{self, MembersShown, NotifyControl};
use crate::gui::components::{button_with_icon, glyph, icon_button, pair, segment, segmented, tip, Avatar, ButtonKind, Shape as Plate};
use crate::gui::people::card::Float;
use crate::gui::people::info::InfoPane;
use crate::gui::people::presence::{self, Ring, Shape};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};
use protocol::{CallSupport, Conversation, ConversationKind, Key, Network, Notify, Presence, Request};

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
    let mut chips = div().flex().flex_wrap().gap(px(4.)).child(segmented().child(choice(0, format!("Default ({inherited_name})"), None, cx)));
    let mut set = segmented();
    for (ix, (word, name)) in names.iter().enumerate() {
        set = set.child(choice(ix + 1, (*name).to_owned(), Some(*word), cx));
    }
    chips = chips.child(set);
    let muted = (row.muted).then(|| div().type_small().text_color(theme::text_faint()).child("Muted conversations make no sound."));
    div().flex().flex_col().gap(px(6.)).child(div().type_ui().child("Sound")).child(chips).children(muted)
}

/// Notifications' segmented choice for a channel.
fn notify_segments(pane: &InfoPane, row: &Conversation, cx: &mut Context<InfoPane>) -> Div {
    let shown = pane.notify_shown(row);
    // Teams stores anything other than All as muted, so Mentions would silence the channel.
    let teams = row.key.network == Network::Teams;
    let now = if teams && shown == Notify::Mentions { Notify::Nothing } else { shown };
    let choice = |ix: usize, words: &'static str, notify: Notify, cx: &mut Context<InfoPane>| {
        let key = row.key.clone();
        segment(("details-notify", ix), words, now == notify)
            .px(px(8.))
            .on_click(cx.listener(move |pane, _, _, cx| pane.notify_to(key.clone(), notify, cx)))
    };
    let mut choices = segmented().child(choice(0, "All", Notify::All, cx));
    if !teams {
        choices = choices.child(choice(1, "Mentions", Notify::Mentions, cx));
    }
    choices.child(choice(2, "Nothing", Notify::Nothing, cx))
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
    if let Some(call) = call {
        top = top.child(div().mt(px(16.)).flex().items_center().gap(px(8.)).child(call).children(more));
    }
    let control = facts::notify_control(row);
    let notify = pane.notify_shown(row);
    let key = row.key.clone();
    top = top.child(
        draw::notifications_row(match control {
            NotifyControl::Switch => draw::switch("details-switch", notify != Notify::Nothing)
                .on_click(cx.listener(move |pane, _, _, cx| {
                    let to = if notify == Notify::Nothing { Notify::All } else { Notify::Nothing };
                    pane.notify_to(key.clone(), to, cx)
                }))
                .into_any_element(),
            NotifyControl::Segmented => notify_segments(pane, row, cx).into_any_element(),
            NotifyControl::Held(words) => div().type_small().text_color(theme::text_soft()).child(words).into_any_element(),
        })
        .mt(px(12.)),
    );
    top = top.child(sound_row(row, cx).mt(px(8.)));
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
    match facts::members_shown(row, info) {
        MembersShown::Inline => {
            let (hub, ..) = pane.parts();
            let (_, _, _, _, _, members) = pane.parts();
            if !members.is_empty() {
                sections = sections.child(draw::label("Members", !any));
                any = true;
                let ring = Ring::on(theme::sidebar());
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
                            .child(div().min_w_0().truncate().type_ui().child(person.name.clone()))
                            .children((!word.is_empty()).then(|| div().flex_none().type_small().text_color(theme::text_faint()).child(format!("· {word}"))))
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
    if !waiting || late {
        body = body.child(draw::rule().mt(px(16.))).child(pane.shared_of(network, window, cx).mt(px(8.)));
    }
    let created = info.map(facts::created_line).unwrap_or_default();
    body.child(
        div()
            .px(px(theme::PANE_PAD))
            .pt(px(16.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(draw::foot_line(created))
            .children(link.filter(|_| facts::foot_opens(network)).map(|url| draw::open_in("details-open", network).on_click(move |_, _, cx| cx.open_url(&url)))),
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
        CallSupport::None => None,
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
pub fn space(pane: &InfoPane, key: &Key, cx: &mut Context<InfoPane>) -> Div {
    let (hub, _, _, info, known, _) = pane.parts();
    let info = info.filter(|info| info.space.key == *key).cloned();
    let name = known.map(|space| space.name.clone()).or_else(|| info.as_ref().map(|info| info.space.name.clone())).unwrap_or_default();
    let icon = known.and_then(|space| space.icon.clone()).map(|icon| hub.file(&key.account(), &icon, cx));
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
        _ => None,
    };
    let top = div()
        .px(px(theme::PANE_PAD))
        .flex()
        .flex_col()
        .child(div().pt(px(16.)).child(Avatar::new(key, &name, draw::AVATAR).shape(Plate::Place).image(icon.as_ref()).build()))
        .child(div().mt(px(12.)).min_w_0().truncate().type_title().child(name.clone()))
        .children(line.map(|line| line.type_ui().text_color(theme::text_soft())));
    // CONTRACT: no request sets a Space's notifications, so the setting `SpaceInfo` gives is
    // shown as it is, read-only, and changed in the network's app.
    let notify = info.as_ref().filter(|_| key.network == Network::Discord).map(|info| info.notify);
    let top = top.children(notify.map(|notify| {
        let shown = |ix: usize, words: &'static str, at: Notify| segment(("space-notify", ix), words, notify == at).px(px(8.)).cursor_default();
        div()
            .id("space-notify-row")
            .mt(px(12.))
            .tooltip(tip(format!("Change this in {}", key.network.label()), None))
            .child(draw::notifications_row(
                segmented()
                    .child(shown(0, "All", Notify::All))
                    .child(shown(1, "Mentions", Notify::Mentions))
                    .child(shown(2, "Nothing", Notify::Nothing)),
            ))
    }));
    let mut body = div().flex().flex_col().pb(px(theme::PANE_PAD)).child(top);
    let mut sections = div().px(px(theme::PANE_PAD)).flex().flex_col();
    let mut any = false;
    if let Some(info) = &info {
        if !info.description.trim().is_empty() {
            any = true;
            sections = sections.child(draw::label("About", true)).child(div().type_ui().line_clamp(6).child(info.description.trim().to_owned()));
        }
        if !info.nickname.is_empty() || !info.roles.is_empty() {
            sections = sections.child(draw::label("Role", !any));
            any = true;
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
    }
    if any {
        body = body.child(draw::rule().mt(px(16.))).child(sections.pt(px(4.)));
    } else if failed {
        body = body.child(draw::rule().mt(px(16.))).child(pane.failed_of(cx));
    } else if waiting && late {
        body = body.child(draw::rule().mt(px(16.))).child(div().px(px(theme::PANE_PAD)).pt(px(16.)).child(draw::skeleton()));
    }
    let link = facts::space_link(key);
    body.child(
        div()
            .px(px(theme::PANE_PAD))
            .pt(px(16.))
            .children(link.map(|url| draw::open_in("space-open", key.network).on_click(move |_, _, cx| cx.open_url(&url)))),
    )
}
