//! Search on a phone: local first. The field is the desktop's own search field, focused the moment
//! the tab is entered; the results are the conversations this phone already holds. Asking the
//! accounts is the next wave (`01-ia-navigation.md` §4.3).

use crate::gui::components::{empty_state, search_field};
use crate::gui::phone::inbox::phone_section;
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::phone::counted;
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, Focusable, Window};
use protocol::Conversation;
use std::rc::Rc;

/// The rows a result list shows before it stops; the rest are one search away.
const SHOWN: usize = 40;

/// Whether a conversation answers a query: its title or its last line holds it, case aside.
/// An empty query answers every conversation.
pub fn matches(row: &Conversation, needle: &str) -> bool {
    needle.is_empty() || row.title.to_lowercase().contains(needle) || row.preview.to_lowercase().contains(needle)
}

impl Shell {
    pub(super) fn phone_search(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let _ = snapshot;
        let (needle, focused) = match &self.phone_state.search {
            Some(field) => (
                field.read(cx).text().trim().to_lowercase(),
                field.read(cx).focus_handle(cx).is_focused(window),
            ),
            None => (String::new(), false),
        };
        let rows = self.sidebar_state.inbox.all();
        let found: Vec<&Rc<Conversation>> = rows.iter().filter(|row| matches(row, &needle)).take(SHOWN).collect();
        let heading = if needle.is_empty() {
            format!("On this phone · {}", counted(rows.len(), "conversation", "conversations"))
        } else {
            format!("Matches · {}", counted(found.len(), "conversation", "conversations"))
        };
        let mut list = div()
            .id("phone-search")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb(px(tokens::GUTTER))
            .flex()
            .flex_col();
        if rows.is_empty() && self.sidebar_state.inbox.fresh {
            list = list.child(self.phone_loading(cx));
        } else if rows.is_empty() {
            list = list.child(empty_state(
                "icons/search.svg",
                "Nothing to search yet",
                "Conversations from the networks you sign in to show up here.",
            ));
        } else if found.is_empty() {
            list = list.child(empty_state("icons/search.svg", "No matches", "Search covers the conversations on this phone."));
        } else {
            list = list.child(phone_section(heading, true));
            for (ix, row) in found.iter().enumerate() {
                list = list.child(self.phone_row(ix, row, ix + 1 == found.len(), cx));
            }
        }
        let field = search_field(focused)
            .mx(px(tokens::GUTTER))
            .mt(px(12.))
            .min_h(px(tokens::TOUCH))
            .child(div().flex_1().min_w_0().phone_row().line_height(px(tokens::scaled(20.))).children(self.phone_state.search.clone()))
            .children((!needle.is_empty()).then(|| {
                div()
                    .flex_none()
                    .phone_preview()
                    .text_color(theme::text_soft())
                    .child("Clear")
                    .id("phone-search-clear")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(field) = this.phone_state.search.clone() {
                            field.update(cx, |field, cx| field.clear(cx));
                        }
                    }))
            }));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(tokens::NAV))
                    .pl(px(tokens::GUTTER))
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

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key, Network};

    fn conversation(title: &str, preview: &str) -> Conversation {
        let key = Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, "c");
        Conversation { preview: preview.to_owned(), ..Conversation::new(key, protocol::ConversationKind::Direct, title) }
    }

    #[test]
    fn a_query_finds_a_conversation_by_its_title_or_its_last_line_and_ignores_case() {
        let row = conversation("Mara", "lunch at noon?");
        assert!(matches(&row, ""));
        assert!(matches(&row, "mar"));
        assert!(matches(&row, "lunch"));
        assert!(matches(&row, "noon"));
        assert!(!matches(&row, "dinner"));
    }
}
