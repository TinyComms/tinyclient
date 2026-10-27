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

    /// My own picture on the account a conversation belongs to.
    pub(super) fn own_picture(&self, conversation: &Key) -> Option<MediaFile> {
        let account = conversation.account();
        let accounts = self.snapshot.iter().flat_map(|snapshot| &snapshot.accounts);
        accounts.filter(|status| status.key == account).find_map(|status| status.avatar.clone())
    }

    /// The strip above the messages (07 §1): panel, back and forward, picture, title and the
    /// line after it on one baseline, the call, then the pane's buttons with Details last.
    pub(super) fn strip(&self, open: &Conversation, link: Link, window: &Window, cx: &mut Context<Self>) -> Div {
        let direct = open.kind == ConversationKind::Direct;
        let picture = match open.kind {
            ConversationKind::Direct | ConversationKind::Group => Avatar::new(&open.key, &open.title, 24.),
            ConversationKind::Voice => Avatar::channel("icons/volume-2.svg", 24.),
            ConversationKind::Announcement => Avatar::channel("icons/megaphone.svg", 24.),
            ConversationKind::Thread | ConversationKind::Forum => Avatar::channel("icons/messages-square.svg", 24.),
            ConversationKind::Channel if open.private => Avatar::channel("icons/lock.svg", 24.),
            ConversationKind::Channel => Avatar::channel("icons/hash.svg", 24.),
        };
        let shape = if open.kind.is_direct() { Shape::Person } else { Shape::Place };
        let picture = picture.shape(shape).image(self.conversation_picture(open, cx).as_ref());
        // A group has one face across the window: the same one its row and the switcher draw.
        let face = if open.kind == ConversationKind::Group {
            self.conversation_avatar(open, 24., theme::canvas(), cx)
        } else {
            picture.build()
        };
        let trouble = match link {
            Link::Connected => None,
            Link::Offline => Some("Reconnecting"),
            Link::Expired => Some("Signed out by the network"),
            Link::SignedOut => Some("Signed out"),
        };
        let subtitle = self.header_line(open);
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
        let members = (!direct).then(|| {
            icon_button("members", "icons/users.svg", self.pane_shows(|layer| matches!(layer, Layer::Members(_))))
                .tooltip(tip("Members", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_member_list(cx)))
        });
        let (glyph, words) = if direct { ("icons/user.svg", "Profile") } else { ("icons/info.svg", "Details") };
        let details = icon_button("details", glyph, self.pane_shows(|layer| matches!(layer, Layer::Profile(_) | Layer::Details(_))))
            .tooltip(tip(words, Some("⌘.")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)));
        // The title keeps 96 before the line after it gives up anything; that line truncates first.
        // The title truncates before anything to its right moves; the line after it goes first.
        let title = div().flex_initial().min_w(px(96.)).truncate().type_title().child(open.title.clone());
        let space = open.space.clone().map(|id| Key { id, ..open.key.clone() });
        let line = (!subtitle.is_empty()).then(|| {
            div()
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
            .child(div().flex_none().ml(px(12.)).mr(px(8.)).child(face))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // Nothing in this row draws over the call or the buttons, however narrow.
                    .overflow_hidden()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .children(mixed.then(|| div().flex_none().mr(px(-2.)).child(network_icon(open.key.network, 14.))))
                    .child(title)
                    .children(mark(open.mark))
                    .children(line)
                    .children(trouble.map(|words| div().flex_none().type_small().text_color(theme::text_faint()).child(words))),
            )
            .children(call.map(|call| div().flex_none().ml(px(8.)).child(call)))
            .child(div().flex_none().ml(px(8.)).flex().gap(px(4.)).children(members).child(details))
    }

    /// The line after the title (07 §1): beside a direct conversation the person's status or
    /// presence; elsewhere where it is (`Space › group`) and its topic.
    fn header_line(&self, open: &Conversation) -> String {
        if open.kind == ConversationKind::Direct {
            if let Some(status) = self.header_status(&open.key) {
                return status.to_string();
            }
        }
        path_line(&open.subtitle())
    }
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
