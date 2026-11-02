//! The strip above the messages: where this is, and the few controls that act on the whole
//! conversation. The buttons belong to the slices that own what they do; this places them.

use crate::gui::pane::Layer;
use crate::gui::components::{glyph, icon_button, network_icon, tip, Avatar, Shape};
use crate::gui::chrome::{self, End};
use crate::gui::layout::{drags_window, TOP_BAR};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};
use protocol::{Conversation, ConversationKind, Key, Link, Mark, MediaFile};

impl Shell {
    /// The picture of a conversation: its own, or for a direct one the other person's as the
    /// people hub knows it. Asked for when it is not on disk.
    pub(super) fn conversation_picture(&self, open: &Conversation, cx: &mut Context<Self>) -> Option<MediaFile> {
        let hub = self.people_state.hub();
        let peer = open.peer.as_ref().map(|peer| Key { id: peer.clone(), ..open.key.clone() });
        let file = open.avatar.clone().or_else(|| peer.and_then(|peer| hub.person(&peer)).and_then(|person| person.avatar));
        hub.avatar(&open.key.account(), file.as_ref(), cx)
    }

    /// Where that picture is on disk. A Tiny member's picture, already fetched for the server
    /// page, is used when the people hub has none. The bytes are not logged.
    pub(in crate::gui) fn conversation_face(&self, open: &Conversation, cx: &mut Context<Self>) -> Option<std::path::PathBuf> {
        if let Some(path) = self.conversation_picture(open, cx).and_then(|file| file.path).filter(|path| !path.is_empty()) {
            return Some(std::path::PathBuf::from(path));
        }
        let id = open.peer.as_deref().filter(|id| !id.is_empty())?;
        self.tiny_member_picture(&open.key.account(), id)
    }

    /// My own picture on the account a conversation belongs to.
    pub(super) fn own_picture(&self, conversation: &Key) -> Option<MediaFile> {
        let account = conversation.account();
        let accounts = self.snapshot.iter().flat_map(|snapshot| &snapshot.accounts);
        accounts.filter(|status| status.key == account).find_map(|status| status.avatar.clone())
    }

    /// The strip above the messages (07 §1): panel, back and forward, picture, title and the
    /// line after it on one center, the call, then search and the pane's buttons, Details last.
    /// The encryption chip carries an icon, so the row centers instead of sharing a text
    /// baseline: a baseline puts that chip below the name and the presence word.
    pub(super) fn strip(&self, open: &Conversation, link: Link, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let direct = open.kind == ConversationKind::Direct;
        let teams_channel = open.key.network == protocol::Network::Teams && open.kind == ConversationKind::Channel;
        let saved = open.special == protocol::Special::Saved && open.key.network == protocol::Network::Telegram;
        let picture = match open.kind {
            ConversationKind::Direct | ConversationKind::Group => Avatar::new(&open.key, &open.title, 24.),
            ConversationKind::Voice => Avatar::channel("icons/volume-2.svg", 24.),
            ConversationKind::Announcement => Avatar::channel("icons/megaphone.svg", 24.),
            ConversationKind::Thread | ConversationKind::Forum => Avatar::channel("icons/messages-square.svg", 24.),
            ConversationKind::Channel if open.private => Avatar::channel("icons/lock.svg", 24.),
            ConversationKind::Channel => Avatar::channel("icons/hash.svg", 24.),
        };
        let shape = if open.kind.is_direct() { Shape::Person } else { Shape::Place };
        let picture = picture.shape(shape).image_at(self.conversation_face(open, cx));
        // A direct conversation and a group share one face with their row: the picture, and
        // for a person the presence dot.
        let face = if direct || open.kind == ConversationKind::Group || teams_channel {
            self.conversation_avatar(open, 24., theme::canvas(), cx)
        } else if saved {
            Avatar::channel("icons/bookmark.svg", 24.).build()
        } else {
            picture.build()
        };
        let trouble = match link {
            Link::Connected => None,
            Link::Offline => Some("Reconnecting"),
            Link::Expired => Some("Signed out by the network"),
            Link::SignedOut => Some("Signed out"),
        };
        let beside = self.beside(open);
        let role = self.tiny_header_role(open);
        // The network's icon leads a title only where networks are mixed: a conversation in
        // no Space was reached from the Inbox, which lists every network together. One with a
        // Space says where it is in words.
        let mixed = open.space.is_none();
        let (back, forward) = (self.can_travel(true), self.can_travel(false));
        let panel = self.panel_button(window, cx).map(|button| div().flex_none().mr(px(8.)).child(button));
        // A control, not a pill (polish/01): the system's radius and the strip's 28. Under 600
        // of conversation it keeps only its glyph, and the tooltip keeps the words (07 §1).
        let room = self.conversation_room();
        let compact = room > 0. && room < 600.;
        let search = icon_button("search", "icons/search.svg", self.pane_shows(|layer| matches!(layer, Layer::Search)))
            .tooltip(tip("Search in conversation", Some("⌘F")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_conversation_search(window, cx)));
        let call = self.snapshot.as_ref().and_then(|snapshot| self.call_pill(snapshot, cx)).map(|call| {
            let call = call.h(px(28.)).rounded(px(theme::R_CONTROL)).whitespace_nowrap();
            if compact {
                call.w(px(28.)).px(px(6.)).overflow_hidden()
            } else {
                call
            }
        });
        let travel = |id: &'static str, glyph: &'static str, back: bool, can: bool, words: &'static str, keys: &'static str| {
            icon_button(id, glyph, false)
                .size(px(24.))
                .when(!can, |button| button.opacity(0.35).cursor_default())
                .tooltip(tip(words, Some(keys)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.travel(back, cx)))
        };
        let travel = div()
            .flex_none()
            .flex()
            .gap(px(2.))
            .child(travel("back", "icons/chevron-left.svg", true, back, "Back", "⌘["))
            .child(travel("forward", "icons/chevron-right.svg", false, forward, "Forward", "⌘]"));
        // Lit means "this kind is in the pane now". In a channel, Members and Details are two
        // values of one switch; beside a direct conversation Details is the other person.
        let members = (!direct && !teams_channel).then(|| {
            icon_button("members", "icons/users.svg", self.pane_shows(|layer| matches!(layer, Layer::Members(_))))
                .tooltip(tip("Members", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_member_list(cx)))
        });
        // Beside a channel or group Details has a button; beside a direct conversation the title
        // is the way to the person, a link that opens their profile, so no button is drawn there.
        let details = (!direct).then(|| {
            icon_button("details", "icons/info.svg", self.pane_shows(|layer| matches!(layer, Layer::Details(_))))
                .tooltip(tip("Details", Some("⌘.")))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)))
        });
        // The title keeps 96, or its whole width when shorter, so a short name is not padded
        // away from what follows. The line after it truncates first, then the title.
        let title_tip = if direct { "Open profile".to_owned() } else { open.title.clone() };
        let title = div()
            .id("conversation-title")
            .flex_initial()
            .min_w(title_floor(&open.title, window))
            .truncate()
            .type_title()
            .tooltip(tip(title_tip, direct.then_some("⌘.")))
            .when_some(role.as_ref().and_then(|role| role.color), |title, color| title.text_color(gpui::rgb(color)))
            .child(open.title.clone());
        let title = if direct {
            title
                .cursor_pointer()
                .hover(|style| style.underline())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)))
        } else {
            title
        };
        let space = open.space.clone().map(|id| Key { id, ..open.key.clone() });
        let line = beside.map(|beside| match beside {
            Beside::Person(line) => person_line(&line).into_any_element(),
            Beside::Path(subtitle) => div()
                .id("reveal")
                .flex_1()
                .min_w_0()
                .truncate()
                .type_ui()
                .text_color(theme::text_faint())
                .hover(|style| style.text_color(theme::text_soft()))
                .cursor_pointer()
                .tooltip(tip("Reveal in sidebar", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // CONTRACT: the sidebar has no "reveal this row" hook (slice B); its Space is shown.
                .on_click(cx.listener(move |this, _, _, cx| match space.clone() {
                    Some(space) => this.go(crate::gui::sidebar::Mode::Space(space), cx),
                    None => this.go(crate::gui::sidebar::Mode::Inbox, cx),
                }))
                .child(subtitle)
                .into_any_element(),
        });
        drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .pl(px(theme::PANE_PAD))
            .pr(px(chrome::end_pad(End::Conversation, 12.)))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme::hairline())
            .children(panel)
            .child(travel)
            .child(
                div()
                    .id("conversation-face")
                    .flex_none()
                    .ml(px(12.))
                    .mr(px(8.))
                    .when_some(self.presence_words(open), |face, words| face.tooltip(tip(words, None)))
                    .child(face),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // Nothing in this row draws over the call or the buttons, however narrow.
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .children(mixed.then(|| div().flex_none().self_center().mr(px(-2.)).child(network_icon(open.key.network, 14.))))
                    .child(title)
                    .children(mark(open.mark))
                    .children(role.map(|role| role_chip(&role.name, role.color)))
                    .children(self.encryption_chip(open, cx))
                    .children(line)
                    .children(trouble.map(|words| div().flex_none().type_small().text_color(theme::text_faint()).child(words))),
            )
            .children(call.map(|call| div().flex_none().ml(px(8.)).child(call)))
            .child(div().flex_none().ml(px(8.)).flex().gap(px(4.)).child(search).children(members).children(details))
    }

    /// Whether the conversation is end-to-end encrypted, where its network says: a lock and the
    /// words, or the plain words that the server can read it. A plaintext conversation keeps that
    /// label for good; it is never called encrypted. Opens Encryption, where the safety numbers are.
    fn encryption_chip(&self, open: &Conversation, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        if open.encryption.is_unknown() {
            return None;
        }
        let sealed = open.encryption == protocol::Encryption::EndToEnd;
        // Green when only the members' devices can read it. Amber when the server can.
        // A plaintext conversation is never drawn with the lock.
        let (icon, tone) = if sealed { ("icons/lock.svg", theme::online()) } else { ("icons/triangle-alert.svg", theme::warning()) };
        let account = open.key.account();
        Some(
            div()
                .id("conversation-encryption")
                .flex_none()
                .self_center()
                .h(px(18.))
                .flex()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .type_small()
                .text_color(tone)
                .hover(|style| style.text_color(theme::text()))
                .tooltip(tip(open.encryption.explained(), None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_encryption(&account, window, cx)))
                .child(glyph(icon, 12., tone))
                .child(open.encryption.words()),
        )
    }

    /// The line after the title (07 §1): beside a direct conversation the person's status or
    /// presence; elsewhere where it is (`Space › group`) and its topic.
    fn beside(&self, open: &Conversation) -> Option<Beside> {
        if open.kind == ConversationKind::Direct {
            if let Some(mut line) = self.peer_line(&open.key) {
                // The avatar's dot says it; the word is its tooltip. A call and Telegram's
                // "last seen" have no dot of their own here, so they keep their words.
                if self.presence_dot(open).is_some_and(|shape| shape != crate::gui::people::presence::Shape::InCall) {
                    line.word = "";
                }
                return (!line.word.is_empty() || !line.extra.is_empty()).then_some(Beside::Person(line));
            }
        }
        let path = path_line(&open.subtitle());
        (!path.is_empty()).then(|| Beside::Path(path))
    }
}

/// The most a title holds on to as the row narrows: 96, or all of it when it is shorter.
fn title_floor(title: &str, window: &Window) -> gpui::Pixels {
    let mut style = window.text_style();
    style.font_weight = theme::SEMIBOLD;
    let run = style.to_run(title.len());
    let width = window.text_system().shape_line(title.to_owned().into(), theme::sized(16.), &[run]).width;
    width.ceil().min(px(96.))
}

/// What follows the title. A person's presence is not a place, so it does not reveal the sidebar.
enum Beside {
    Person(crate::gui::people::PeerLine),
    Path(String),
}

/// The presence word in its color, then what they say. Online is green.
fn person_line(line: &crate::gui::people::PeerLine) -> Div {
    let mut row = div().flex_1().min_w_0().flex().items_center().gap(px(6.)).overflow_hidden();
    if !line.word.is_empty() {
        row = row.child(div().flex_none().type_ui().text_color(line.tone).child(line.word));
    }
    if !line.extra.is_empty() {
        if !line.word.is_empty() {
            row = row.child(div().flex_none().type_ui().text_color(theme::text_faint()).child("·"));
        }
        row = row.child(div().min_w_0().truncate().type_ui().text_color(theme::text_faint()).child(line.extra.clone()));
    }
    row
}

/// The other person's role on this server. One chip, the most senior. No email.
fn role_chip(name: &str, color: Option<u32>) -> Div {
    let tone = color.map(gpui::rgb).unwrap_or(theme::text_soft());
    div()
        .flex_none()
        .self_center()
        .h(px(18.))
        .px(px(6.))
        .rounded(px(9.))
        .bg(theme::hover())
        .flex()
        .items_center()
        .gap(px(4.))
        .child(div().size(px(6.)).flex_none().rounded_full().bg(tone))
        .child(div().type_small().text_color(theme::text()).child(name.to_owned()))
}

/// One path glyph everywhere: `›`.
// CONTRACT: `Conversation::subtitle` joins with " > "; the protocol could say "›" itself.
pub(super) fn path_line(subtitle: &str) -> String {
    subtitle.replace(" > ", " › ")
}

/// What the network says the other side is, as a tag after the title. A verified account is
/// a check; the rest are words, red where they warn.
fn mark(mark: Option<Mark>) -> Option<Div> {
    let (words, color) = match mark? {
        Mark::Verified => return Some(div().flex_none().self_center().child(glyph("icons/check.svg", 14., theme::accent_text()))),
        Mark::Scam => ("Scam", theme::danger()),
        Mark::Fake => ("Fake", theme::danger()),
        Mark::Guest => ("Guest", theme::text_soft()),
        Mark::External => ("External", theme::text_soft()),
        Mark::Deactivated => ("Deactivated", theme::text_faint()),
    };
    Some(
        div()
            .flex_none()
            .self_center()
            .px(px(4.))
            .rounded(px(theme::R_CHIP))
            .border_1()
            .border_color(color)
            .text_size(px(11.))
            .line_height(px(14.))
            .font_weight(theme::MEDIUM)
            .text_color(color)
            .child(words),
    )
}

#[cfg(test)]
mod tests {
    use super::path_line;

    #[test]
    fn the_path_reads_with_one_glyph() {
        assert_eq!(path_line("Moss & Lichen > Trail · Maps and routes"), "Moss & Lichen › Trail · Maps and routes");
        assert_eq!(path_line("a>b"), "a>b");
    }
}
