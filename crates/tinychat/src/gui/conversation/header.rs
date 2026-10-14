//! The strip above the messages: where this is, and the few controls that act on the whole
//! conversation. The buttons belong to the slices that own what they do; this places them.

use crate::gui::components::{icon_button, network_icon, tip, Avatar, Shape};
use crate::gui::layout::{drags_window, RightKind, TOP_BAR};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};
use protocol::{Conversation, ConversationKind, Key, Link, MediaFile};

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
        let trouble = match link {
            Link::Connected => None,
            Link::Offline => Some("Reconnecting"),
            Link::Expired => Some("Signed out by the network"),
            Link::SignedOut => Some("Signed out"),
        };
        let subtitle = open.subtitle();
        // The network's icon leads a title only where networks are mixed: a conversation in
        // no Space was reached from the Inbox, which lists every network together. One with a
        // Space says where it is in words.
        let mixed = open.space.is_none();
        let (back, forward) = (self.can_travel(true), self.can_travel(false));
        let panel = self.panel_button(window, cx);
        let call = self.snapshot.as_ref().and_then(|snapshot| self.call_pill(snapshot, cx));
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
        // A channel lists its members there; a direct conversation shows the other person.
        let members = Some(if direct {
            icon_button("members", "icons/user.svg", self.right_kind() == Some(RightKind::Info))
                .tooltip(tip("Profile", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_info(cx)))
        } else {
            icon_button("members", "icons/users.svg", self.right_kind() == Some(RightKind::Members))
                .tooltip(tip("Members", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_members(cx)))
        });
        let title = div().min_w_0().truncate().child(open.title.clone());
        let words = if subtitle.is_empty() {
            div().min_w_0().child(title.type_title())
        } else {
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(title.type_body().font_weight(theme::SEMIBOLD))
                .child(div().truncate().type_small().text_color(theme::text_faint()).child(subtitle))
        };
        drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .px(px(theme::PANE_PAD))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(theme::hairline())
            .children(panel)
            .child(travel)
            .child(picture.build())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .children(mixed.then(|| network_icon(open.key.network, 14.)))
                    .child(words)
                    .children(trouble.map(|words| div().flex_none().type_small().text_color(theme::text_faint()).child(words))),
            )
            .children(members)
            .children(call)
    }
}
