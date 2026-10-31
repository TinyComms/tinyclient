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
        let shown = rows.iter().take(12).collect::<Vec<_>>();
        for (ix, row) in shown.iter().enumerate() {
            list = list.child(self.phone_row(ix, row, ix + 1 == shown.len(), cx));
        }
        let field = div()
            .mx(px(tokens::GUTTER))
            .mt(px(12.))
            .min_h(px(tokens::TOUCH))
            .px(px(12.))
            .rounded(px(12.))
            // The page's own ladder: a field is raised, where the list behind it is the page.
            .bg(tokens::raised())
            .border_1()
            .border_color(tokens::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(glyph("icons/search.svg", 16., theme::text_faint()))
            .child(div().flex_1().min_w_0().truncate().phone_row().text_color(theme::text_faint()).child("Search messages"));
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
            // Same clip as the inbox: the field and title stay above a scrolled row.
            .child(div().flex_1().min_h_0().min_w_0().flex().flex_col().overflow_hidden().child(list))
    }
}
