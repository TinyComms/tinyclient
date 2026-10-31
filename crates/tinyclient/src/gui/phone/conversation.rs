//! A conversation pushed over the list: a 44 pt back chevron, the title, and Details. Its body
//! is the desktop pane (`conversation::Pane`) at `Metrics::PHONE`; the strip and the back
//! gesture are the phone's (`01-ia-navigation.md` §3.2, §5.1). The connection's state is a
//! banner under the strip, never in it, so the title keeps the width it needs.

use crate::gui::components::{banner, banner_action, icon_button, network_icon, Tone};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::state::Shell;
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
    /// The strip, and under it the banner when the link is in trouble. The banner is 32 pt inside
    /// 8 of air, so it reads as a line of state, not a second title.
    pub(in crate::gui) fn phone_strip(&self, open: &Conversation, link: Link, _window: &Window, cx: &mut Context<Self>) -> Div {
        let title = open.title.trim_start_matches('#').to_owned();
        let direct = matches!(open.kind, ConversationKind::Direct | ConversationKind::Group);
        // The face the desktop strip draws, at 32: the conversation's own picture, never a
        // generic person glyph. Channels keep their space mark and the info button.
        let face = direct.then(|| {
            div()
                .flex_none()
                .ml(px(4.))
                .mr(px(2.))
                .child(self.conversation_avatar(open, 32., tokens::surface(), cx))
        });
        let strip = div()
            .flex_none()
            .h(px(tokens::NAV))
            .pl(px(4.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(4.))
            .bg(tokens::surface())
            .shadow(tokens::top_highlight())
            .border_b_1()
            .border_color(tokens::hairline())
            .child(
                icon_button("phone-back", "icons/chevron-left.svg", false)
                    .size(px(tokens::TOUCH))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.phone_back(cx);
                    })),
            )
            .children(face)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children((!direct).then(|| div().flex_none().child(network_icon(open.key.network, 14.))))
                    .child(div().flex_1().min_w_0().truncate().phone_title().child(title)),
            )
            .child(
                icon_button("phone-details", if direct { "icons/user.svg" } else { "icons/info.svg" }, false)
                    .size(px(tokens::TOUCH))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx))),
            );
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
