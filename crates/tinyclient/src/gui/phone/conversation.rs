//! A conversation pushed over the list: a 44 pt back chevron, the title, and Details. Its body
//! is the desktop pane (`conversation::Pane`) at `Metrics::PHONE`; the strip and the back
//! gesture are the phone's (`01-ia-navigation.md` §3.2, §5.1).

use crate::gui::components::{icon_button, network_icon};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::state::Shell;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};
use protocol::{Conversation, ConversationKind, Link};

impl Shell {
    pub(in crate::gui) fn phone_strip(&self, open: &Conversation, link: Link, _window: &Window, cx: &mut Context<Self>) -> Div {
        let trouble = match link {
            Link::Connected => None,
            Link::Offline => Some("Reconnecting"),
            Link::Expired => Some("Signed out by the network"),
            Link::SignedOut => Some("Signed out"),
        };
        let title = open.title.trim_start_matches('#').to_owned();
        let direct = matches!(open.kind, ConversationKind::Direct | ConversationKind::Group);
        div()
            .flex_none()
            .h(px(tokens::NAV))
            .pl(px(4.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(tokens::strip_rule())
            .child(
                icon_button("phone-back", "icons/chevron-left.svg", false)
                    .size(px(tokens::TOUCH))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.phone_back(cx);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children((!direct).then(|| div().flex_none().child(network_icon(open.key.network, 14.))))
                    .child(div().flex_1().min_w_0().truncate().phone_title().child(title))
                    .children(trouble.map(|words| div().flex_none().phone_preview().text_color(theme::text_faint()).child(words))),
            )
            .child(
                icon_button("phone-details", if direct { "icons/user.svg" } else { "icons/info.svg" }, false)
                    .size(px(tokens::TOUCH))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx))),
            )
    }
}
