//! A conversation pushed over the list: the desktop strip with a 44 pt back chevron in place of
//! its travel buttons. Its body is the desktop pane (`conversation::Pane`) at `Metrics::PHONE`.
//! The connection's state is a banner under the strip, never in it, so the title keeps the
//! width it needs.

use crate::gui::components::{banner, banner_action, glyph, icon_button, network_icon, Tone};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::state::Shell;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};
use protocol::{Conversation, ConversationKind, Link};

/// What the connection says under the strip, when it says anything: the words, the tone, and
/// whether the one thing to do about it is to sign in again.
pub fn trouble(link: Link) -> Option<(&'static str, Tone, bool)> {
    match link {
        Link::Connected => None,
        Link::Offline => Some(("Reconnecting", Tone::Warning, false)),
        Link::Expired => Some(("Signed out by the network", Tone::Attention, true)),
        Link::SignedOut => Some(("Signed out", Tone::Attention, true)),
    }
}

impl Shell {
    /// The desktop strip at a phone's size: back where the desktop has its travel buttons, the
    /// conversation's picture and title, the call, then Members and Details as the desktop
    /// strip has them. Under it the banner when the link is in trouble.
    pub(in crate::gui) fn phone_strip(&self, open: &Conversation, link: Link, _window: &Window, cx: &mut Context<Self>) -> Div {
        let direct = open.kind == ConversationKind::Direct;
        let size = 28.;
        // The face its row in the sidebar wears, so the page and the row it came from match.
        let face = self.conversation_avatar(open, size, theme::canvas(), cx);
        // The network's icon leads the title where networks are mixed, as on the desktop.
        let mixed = open.space.is_none();
        let place = open.space_title.trim().to_owned();
        let subtitle = (!direct && !place.is_empty())
            .then(|| div().flex_none().max_w(px(120.)).truncate().phone_stamp().font_weight(theme::REGULAR).text_color(theme::text_faint()).child(place));
        // Whether it is end-to-end encrypted, where the network says: the desktop strip's chip,
        // as the subtitle's line here (a lock and the words, or that the server can read it).
        // A tap opens Encryption, where the safety numbers are.
        let encryption = (!open.encryption.is_unknown()).then(|| {
            let sealed = open.encryption == protocol::Encryption::EndToEnd;
            let (icon, tone) = if sealed { ("icons/lock.svg", theme::text_soft()) } else { ("icons/triangle-alert.svg", theme::text_faint()) };
            let account = open.key.account();
            div()
                .id("phone-encryption")
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(4.))
                .phone_stamp()
                .font_weight(theme::REGULAR)
                .text_color(tone)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_encryption(&account, window, cx)))
                .child(div().flex_none().child(glyph(icon, 11., tone)))
                .child(div().min_w_0().truncate().child(open.encryption.words()))
        });
        // The call is one of the strip's glyphs (`call_pill` draws it so on a phone): 44 pt, no
        // box, beside Members and Details.
        let call = self.snapshot.as_ref().and_then(|snapshot| self.call_pill(snapshot, cx));
        let action = |id: &'static str, icon: &'static str| {
            icon_button(id, icon, false).size(px(tokens::TOUCH)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let members = (!direct).then(|| action("phone-members", "icons/users.svg").on_click(cx.listener(|this, _, _, cx| this.toggle_member_list(cx))));
        let details = action("phone-details", if direct { "icons/user.svg" } else { "icons/info.svg" })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)));
        let strip = div()
            .flex_none()
            .h(px(tokens::NAV))
            .pl(px(2.))
            .pr(px(4.))
            .flex()
            .items_center()
            .bg(theme::canvas())
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                icon_button("phone-back", "icons/chevron-left.svg", false)
                    .size(px(tokens::TOUCH))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.phone_back(Some(window), cx);
                    })),
            )
            .child(div().flex_none().mr(px(10.)).child(face))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .children(mixed.then(|| div().flex_none().child(network_icon(open.key.network, 14.))))
                            .child(div().min_w_0().truncate().phone_title().child(open.title.clone())),
                    )
                    // One line under the title: where it is, then whether it is encrypted.
                    .when(subtitle.is_some() || encryption.is_some(), |column| {
                        column.child(div().min_w_0().flex().items_center().gap(px(6.)).children(subtitle).children(encryption))
                    }),
            )
            .children(call)
            .children(members)
            .child(details);
        let state = trouble(link).map(|(words, tone, sign_in)| {
            let row = banner(tone, words);
            let row = if sign_in {
                // The same sign-in entry the desktop's banner uses for an expired or signed-out
                // network: it opens that network's own window.
                let network = open.key.network;
                row.child(banner_action("phone-sign-in-again", "Sign in").on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx))))
            } else {
                row
            };
            div().flex_none().px(px(8.)).pt(px(8.)).child(row)
        });
        div().flex_none().flex().flex_col().child(strip).children(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_link_in_trouble_gets_a_banner_and_only_a_signed_out_one_offers_sign_in() {
        assert!(trouble(Link::Connected).is_none());
        assert_eq!(trouble(Link::Offline).map(|t| t.0), Some("Reconnecting"));
        assert!(!trouble(Link::Offline).unwrap().2);
        assert!(trouble(Link::SignedOut).unwrap().2);
        assert!(trouble(Link::Expired).unwrap().2);
    }
}
