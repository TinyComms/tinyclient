//! Search on a phone: local first. The field is drawn, the results are the conversations this
//! phone already holds; asking the accounts is the next wave (`01-ia-navigation.md` §4.3).

use crate::gui::components::glyph;
use crate::gui::phone::inbox::phone_section;
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, Window};

impl Shell {
    pub(super) fn phone_search(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let _ = (snapshot, window);
        let rows = self.sidebar_state.inbox.all();
        let mut list = div()
            .id("phone-search")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb(px(tokens::GUTTER))
            .flex()
            .flex_col();
        list = list.child(phone_section(format!("On this phone · {}", rows.len()), true));
        for (ix, row) in rows.iter().take(12).enumerate() {
            list = list.child(self.phone_row(ix, row, cx));
        }
        let field = div()
            .mx(px(tokens::GUTTER))
            .mt(px(12.))
            .h(px(tokens::TOUCH))
            .px(px(12.))
            .rounded(px(12.))
            .bg(theme::field())
            .border_1()
            .border_color(theme::border())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(glyph("icons/search.svg", 18., theme::text_faint()))
            .child(div().phone_row().text_color(theme::text_faint()).child("Search messages"));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(tokens::NAV))
                    .pl(px(16.))
                    .flex()
                    .items_center()
                    .bg(tokens::surface())
                    .shadow(tokens::top_highlight())
                    .border_b_1()
                    .border_color(tokens::hairline())
                    .child(div().phone_title().child("Search")),
            )
            .child(field)
            .child(list)
    }
}
