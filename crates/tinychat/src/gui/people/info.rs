//! A person in the right pane: the profile card's content, docked, with everything the network
//! says of them and, when the open conversation is the one with them, what was shared in it.
//!
//! The pane draws whatever the profile carries. Which labelled lines there are differs by
//! network (a phone number and a username on Telegram, a title and an email on Slack and Teams,
//! "Member since" and mutual servers on Discord), and none of that is decided here.

use super::card::{local_time, own_offset, relation_word, spaces_shown};
use super::list::bot_tag;
use super::presence::{self, Ring, Shape};
use super::{can_call, in_call, only_draws, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{
    button, button_with_icon, glyph, icon_button, network_icon, segment, segmented, Avatar, ButtonKind,
    Shape as Plate,
};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, rgb, ClipboardItem, Context, Div, MouseButton, WeakEntity, Window};
use protocol::{Key, Network, Person, Profile, Request, Response, SharedItem, SharedKind};

/// Shared things read at once. The pane lists the newest; the conversation has the rest.
const SHARED: u32 = 60;
/// Media tiles in a row.
const COLUMNS: usize = 3;
const BANNER: f32 = 72.;

pub struct InfoPane {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    person: Option<Key>,
    /// What the network said of them. The head is drawn from the row before this arrives.
    profile: Option<Profile>,
    asked: u64,
    tab: SharedKind,
    /// The conversation whose shared things are listed, and what it holds of the tab's kind.
    shared_in: Option<Key>,
    shared: Vec<SharedItem>,
    shared_more: bool,
    shared_asked: u64,
}

/// `184 KB`
fn size_words(bytes: u64) -> String {
    match bytes {
        0 => String::new(),
        bytes if bytes < 1024 => format!("{bytes} B"),
        bytes if bytes < 1024 * 1024 => format!("{} KB", bytes.div_ceil(1024)),
        bytes => format!("{:.1} MB", bytes as f64 / (1024. * 1024.)),
    }
}

/// What a link reads as when the network gave it no title: its host and path.
fn link_words(url: &str) -> &str {
    url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/')
}

impl InfoPane {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub) -> InfoPane {
        InfoPane {
            shell,
            hub,
            scene: Scene::default(),
            person: None,
            profile: None,
            asked: 0,
            tab: SharedKind::Media,
            shared_in: None,
            shared: Vec::new(),
            shared_more: false,
            shared_asked: 0,
        }
    }

    pub fn person(&self) -> Option<&Key> {
        self.person.as_ref()
    }

    /// The open conversation moved. What was shared follows it: only the conversation with
    /// this person lists anything.
    pub fn see(&mut self, scene: Scene, cx: &mut Context<Self>) {
        let moved = scene.open != self.scene.open;
        self.scene = scene;
        if moved {
            self.ask_shared(cx);
        }
    }

    /// Puts a person in the pane and asks the network about them.
    pub fn show(&mut self, person: Key, cx: &mut Context<Self>) {
        // Their presence is drawn, so it is watched for as long as the pane shows them.
        self.hub.watch("info", [person.clone()], cx);
        if self.profile.as_ref().is_some_and(|profile| profile.person.key != person) {
            self.profile = None;
        }
        self.person = Some(person.clone());
        self.asked += 1;
        self.ask_shared(cx);
        cx.notify();
        if only_draws() {
            return;
        }
        let (asked, this) = (self.asked, cx.weak_entity());
        super::ask_later(cx, Request::Profile { person, space: None }, move |answer, cx| {
            let Ok(Response::Profile { profile }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.asked == asked {
                    this.hub.learn(std::slice::from_ref(&profile.person));
                    this.profile = Some(*profile);
                    cx.notify();
                }
            });
        });
    }

    /// The pane was closed: nobody in it is drawn any more.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.hub.watch("info", [], cx);
        self.person = None;
    }

    fn pick_tab(&mut self, tab: SharedKind, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.ask_shared(cx);
        }
    }

    /// Reads what the tab lists, from the conversation with this person when that is the one
    /// open. Any other conversation is not theirs to list.
    fn ask_shared(&mut self, cx: &mut Context<Self>) {
        self.shared_asked += 1;
        let with_them = self.person.is_some() && self.scene.peer == self.person;
        let conversation = self.scene.open.clone().filter(|_| with_them);
        if conversation != self.shared_in {
            self.shared.clear();
            self.shared_more = false;
        }
        self.shared_in = conversation.clone();
        cx.notify();
        let Some(conversation) = conversation.filter(|_| !only_draws()) else {
            return;
        };
        let (asked, this) = (self.shared_asked, cx.weak_entity());
        let request = Request::Shared { conversation, kind: self.tab, before: None, limit: SHARED, search: false };
        super::ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Shared { items, more, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.shared_asked == asked {
                    this.shared = items;
                    this.shared_more = more;
                    cx.notify();
                }
            });
        });
    }

    fn message(&mut self, person: &Person, call: bool, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            if call {
                shell.call_person(person, cx);
            } else {
                shell.open_person(person, cx);
            }
        });
    }

    /// One labelled line. A click copies its value.
    fn line(&self, ix: usize, label: String, value: String, cx: &mut Context<Self>) -> impl IntoElement {
        let copied = value.clone();
        div()
            .id(("info-line", ix))
            .min_h(px(theme::ROW_ONE))
            .py(px(4.))
            .flex()
            .items_start()
            .justify_between()
            .gap(px(12.))
            .type_ui()
            .cursor_pointer()
            .child(div().flex_none().text_color(theme::text_soft()).child(label))
            .child(div().min_w_0().text_right().child(value))
            .on_click(cx.listener(move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))))
    }

    /// Everything said of them beyond the head, in one column.
    fn details(&self, person: &Person, profile: &Profile, cx: &mut Context<Self>) -> Div {
        let label = |words: &'static str| {
            div().mt(px(12.)).h(px(20.)).flex().items_center().type_micro().text_color(theme::text_faint()).child(words)
        };
        let mut lines: Vec<(String, String)> = profile.fields.clone();
        if let Some(since) = profile.since {
            lines.push(("Member since".to_owned(), crate::gui::time::date(since)));
        }
        if let Some(offset) = profile.utc_offset {
            let now = crate::view::now();
            lines.push(("Local time".to_owned(), local_time(now, offset, own_offset(now))));
        }
        let discord = person.key.network == Network::Discord;
        let mut details = div().flex().flex_col();
        if !lines.is_empty() {
            let rows = lines.into_iter().enumerate().map(|(ix, (label, value))| self.line(ix, label, value, cx));
            details = details.child(div().mt(px(8.)).flex().flex_col().children(rows));
        }
        if !profile.bio.is_empty() {
            details = details
                .child(label("About"))
                .child(div().type_ui().text_color(theme::text_soft()).child(profile.bio.clone()));
        }
        if profile.mutual_friends > 0 || !profile.mutual_spaces.is_empty() {
            let (shown, more) = spaces_shown(&profile.mutual_spaces);
            let chip = |words: String| {
                div()
                    .h(px(20.))
                    .max_w(px(200.))
                    .px(px(6.))
                    .rounded(px(theme::R_CHIP))
                    .bg(theme::hover())
                    .flex()
                    .items_center()
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(div().truncate().child(words))
            };
            let friends = match profile.mutual_friends {
                0 => None,
                1 => Some("1 mutual friend".to_owned()),
                count => Some(format!("{count} mutual friends")),
            };
            details = details.child(label(if discord { "Mutual" } else { "In common" })).child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(friends.map(chip))
                    .children(shown.iter().cloned().map(chip))
                    .children((more > 0).then(|| chip(format!("+{more}")))),
            );
        }
        details
    }

    /// The tabs and what the open one lists.
    fn shared(&self, person: &Person, cx: &mut Context<Self>) -> Div {
        let tab = |id: &'static str, words: &'static str, kind: SharedKind| {
            segment(id, words, self.tab == kind).on_click(cx.listener(move |this, _, _, cx| this.pick_tab(kind, cx)))
        };
        let tabs = segmented()
            .child(tab("info-media", "Media", SharedKind::Media))
            .child(tab("info-files", "Files", SharedKind::Files))
            .child(tab("info-links", "Links", SharedKind::Links));
        let account = person.key.account();
        let empty = |words: &'static str| div().py(px(16.)).type_ui().text_color(theme::text_faint()).child(words);
        let body = match (self.tab, self.shared.is_empty()) {
            (SharedKind::Media, true) => empty("No pictures or videos shared here yet."),
            (SharedKind::Files, true) => empty("No files shared here yet."),
            (SharedKind::Links, true) => empty("No links shared here yet."),
            (SharedKind::Media, false) => {
                let tiles = self.shared.iter().filter_map(|item| {
                    let attachment = item.attachment.as_ref()?;
                    let shown = attachment.thumb.as_ref().unwrap_or(&attachment.file);
                    let picture = self.hub.file(&account, shown, cx);
                    Some(
                        div()
                            .w(gpui::relative(1. / COLUMNS as f32))
                            .p(px(2.))
                            .child(Avatar::named("", 96.).shape(Plate::Place).image(Some(&picture)).build()),
                    )
                });
                div().mx(px(-2.)).flex().flex_wrap().children(tiles)
            }
            (SharedKind::Files, false) => {
                let rows = self.shared.iter().enumerate().filter_map(|(ix, item)| {
                    let attachment = item.attachment.as_ref()?;
                    let on_disk = attachment.file.path.clone();
                    let said = [size_words(attachment.file.bytes), item.sender.clone(), crate::gui::time::date(item.sent)];
                    let said = said.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
                    Some(
                        div()
                            .id(("info-file", ix))
                            .h(px(theme::ROW_TWO))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .when(on_disk.is_some(), |row| row.cursor_pointer())
                            .child(glyph("icons/file.svg", 16., theme::text_soft()))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().truncate().type_ui().child(attachment.name.clone()))
                                    .child(div().truncate().type_small().text_color(theme::text_faint()).child(said)),
                            )
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(path) = &on_disk {
                                    cx.open_url(&format!("file://{path}"));
                                }
                            })),
                    )
                });
                div().flex().flex_col().children(rows)
            }
            (SharedKind::Links, false) => {
                let rows = self.shared.iter().enumerate().map(|(ix, item)| {
                    let url = item.url.clone();
                    let title = if item.title.is_empty() { link_words(&item.url).to_owned() } else { item.title.clone() };
                    div()
                        .id(("info-link", ix))
                        .h(px(theme::ROW_TWO))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .child(glyph("icons/link.svg", 16., theme::text_soft()))
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().type_ui().child(title))
                                .child(
                                    div()
                                        .truncate()
                                        .type_small()
                                        .text_color(theme::text_faint())
                                        .child(link_words(&item.url).to_owned()),
                                ),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                });
                div().flex().flex_col().children(rows)
            }
        };
        div()
            .mt(px(16.))
            .pt(px(12.))
            .border_t_1()
            .border_color(theme::hairline())
            .flex()
            .flex_col()
            .child(div().flex().child(tabs))
            .child(div().mt(px(8.)).child(body))
            .when(self.shared_more, |shared| {
                shared.child(
                    div().py(px(8.)).type_small().text_color(theme::text_faint()).child("Older ones are in the conversation."),
                )
            })
    }
}

impl Render for InfoPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let known = self.person.as_ref().and_then(|key| {
            let profiled = self.profile.as_ref().filter(|profile| profile.person.key == *key);
            self.hub.person(key).or_else(|| profiled.map(|profile| profile.person.clone()))
        });
        let Some(person) = known else {
            let words = "Click a person to see who they are.";
            return div().size_full().p(px(theme::PANE_PAD)).type_ui().text_color(theme::text_faint()).child(words);
        };
        let network = person.key.network;
        let profile = self.profile.as_ref().filter(|profile| profile.person.key == person.key).cloned();
        let shown = shown_presence(&self.hub, &person);
        let calling = in_call(&self.hub, &self.scene, &person.key);
        let status = self.hub.status_of(&person.key);
        let status = if status.is_empty() { person.status.clone() } else { status };
        let word = if calling { "In a call" } else { presence::word(shown, network) };
        let shape = if calling { Some(Shape::InCall) } else { Shape::of(shown) };
        let said = match (word.is_empty(), status.is_empty()) {
            (true, true) => String::new(),
            (true, false) => status,
            (false, true) => word.to_owned(),
            (false, false) => format!("{word} · {status}"),
        };
        let handle = person.handle.trim_start_matches('@').to_owned();
        let banner = profile.as_ref().and_then(|profile| profile.accent).map_or(theme::hover(), rgb);
        let ring = Ring::on(theme::sidebar());
        let (message, call) = (person.clone(), person.clone());
        let (more, shell) = (person.key.clone(), self.shell.clone());
        let link = (!person.link.is_empty()).then(|| person.link.clone());
        let with_them = self.shared_in.is_some();

        let head = div()
            .flex()
            .flex_col()
            .child(div().h(px(BANNER)).bg(banner))
            .child(
                div()
                    .px(px(theme::PANE_PAD))
                    .mt(px(-40.))
                    .flex()
                    .items_end()
                    .justify_between()
                    .child(
                        div()
                            .p(px(4.))
                            .rounded(px(44.))
                            .bg(theme::sidebar())
                            .child(person_avatar(&self.hub, &person, 80., &ring, calling, cx)),
                    )
                    .child(
                        div()
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::hover())
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .type_small()
                            .text_color(theme::text_soft())
                            .child(network_icon(network, 14.))
                            .child(network.label()),
                    ),
            );
        let named = div()
            .mt(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().min_w_0().truncate().type_title().child(person.name.clone()))
            .when(person.bot, |name| name.child(bot_tag()));
        let actions = div()
            .mt(px(12.))
            .flex()
            .gap(px(8.))
            .child(
                button_with_icon("info-message", ButtonKind::Primary, "icons/message-circle.svg", "Message")
                    .flex_1()
                    .on_click(cx.listener(move |this, _, _, cx| this.message(&message, false, cx))),
            )
            .when(can_call(network) && !person.bot, |actions| {
                actions.child(
                    button_with_icon("info-call", ButtonKind::Secondary, "icons/phone.svg", "Call")
                        .flex_1()
                        .on_click(cx.listener(move |this, _, _, cx| this.message(&call, true, cx))),
                )
            })
            .child(icon_button("info-more", "icons/ellipsis.svg", false).on_mouse_down(
                MouseButton::Left,
                move |event, _, cx| {
                    cx.stop_propagation();
                    let _ = shell.update(cx, |shell, cx| shell.open_row_menu(more.clone(), event.position, cx));
                },
            ));

        let body = div()
            .px(px(theme::PANE_PAD))
            .pb(px(theme::PANE_PAD))
            .flex()
            .flex_col()
            .child(named)
            .when(!handle.is_empty(), |body| {
                body.child(div().truncate().type_ui().text_color(theme::text_soft()).child(format!("@{handle}")))
            })
            .when(!said.is_empty(), |body| {
                body.child(
                    div()
                        .mt(px(6.))
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        .type_ui()
                        .text_color(theme::text_soft())
                        .children(shape.map(|shape| div().pt(px(4.)).child(presence::inline(shape, 10., theme::sidebar()))))
                        .child(div().min_w_0().line_clamp(3).child(said)),
                )
            })
            .children(relation_word(person.relation).map(|word| {
                div().mt(px(4.)).type_small().text_color(theme::text_soft()).child(word)
            }))
            .child(actions)
            .children(profile.as_ref().map(|profile| self.details(&person, profile, cx)))
            .when(with_them, |body| body.child(self.shared(&person, cx)))
            .child(
                div()
                    .mt(px(16.))
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(theme::hairline())
                    .flex()
                    .flex_col()
                    .items_start()
                    .children(link.map(|url| {
                        button("info-open", ButtonKind::Ghost, format!("Open in {}", network.label()))
                            .h(px(24.))
                            .px(px(6.))
                            .type_small()
                            .child(glyph("icons/external-link.svg", 14., theme::text_soft()))
                            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                    }))
            );

        div().size_full().child(div().id("info-scroll").size_full().overflow_y_scroll().child(head).child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_links_read_as_a_person_would_say_them() {
        assert_eq!(size_words(0), "");
        assert_eq!(size_words(900), "900 B");
        assert_eq!(size_words(188_416), "184 KB");
        assert_eq!(size_words(19_293_798), "18.4 MB");
        assert_eq!(link_words("https://example.org/wiki/Lepraria/"), "example.org/wiki/Lepraria");
    }
}
