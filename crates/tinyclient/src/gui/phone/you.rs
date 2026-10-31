//! You: the account home, one tap from the tab bar. The rows open the desktop settings window
//! today; on a device they become pushed routes (`02-component-inventory.md` F9, E8).

use crate::gui::components::{glyph, Avatar};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::settings::{self, Section};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, Window};
use protocol::Network;
use std::collections::HashSet;

impl Shell {
    pub(super) fn phone_you(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let _ = window;
        let networks: HashSet<Network> = snapshot.accounts.iter().map(|account| account.key.network).collect();
        // My name: who the people hub knows me as on the first account with a picture, as the
        // rail's own avatar says it. `status.me` is an id, never a name.
        let account = snapshot.accounts.iter().find(|account| account.avatar.is_some()).or_else(|| snapshot.accounts.first());
        let (name, avatar) = match account {
            Some(account) => {
                let key = protocol::Key::new(&account.key, account.me.clone());
                let name = self.people_state.hub().person(&key).map_or_else(|| account.name.clone(), |person| person.name);
                let avatar = Avatar::new(&key, &name, 64.)
                    .image_at(self.person_picture(&key, cx).or_else(|| self.own_avatar(&account.key)))
                    .build();
                (name, avatar)
            }
            None => ("tinyclient".to_owned(), Avatar::named("tinyclient", 64.).build()),
        };
        let line = format!("{} accounts · {} networks", snapshot.accounts.len(), networks.len());
        let rows = [
            (Section::Accounts, "Accounts"),
            (Section::General, "Notifications"),
            (Section::Appearance, "Appearance"),
            (Section::Storage, "Data & storage"),
            (Section::About, "About"),
        ];
        let rows = rows.into_iter().enumerate().map(|(ix, (section, words))| {
            div()
                .id(("you-row", ix))
                .h(px(tokens::row_height()))
                .px(px(tokens::GUTTER))
                .flex()
                .items_center()
                .gap(px(12.))
                .cursor_pointer()
                .when(ix > 0, |row| row.border_t_1().border_color(tokens::hairline()))
                .active(|row| row.bg(tokens::pressed()))
                .child(glyph(section.icon(), 20., theme::text_soft()))
                .child(div().flex_1().min_w_0().truncate().phone_row().text_color(theme::text()).child(words))
                .child(glyph("icons/chevron-right.svg", 18., theme::text_faint()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this;
                    settings::open(section, cx);
                }))
        });
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
                    .child(div().phone_title().child("You")),
            )
            .child(
                div()
                    .px(px(tokens::GUTTER))
                    .py(px(24.))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(avatar)
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(div().truncate().phone_title().child(name))
                            .child(div().phone_preview().text_color(theme::text_soft()).child(line)),
                    ),
            )
            .child(div().h(px(1.)).bg(tokens::hairline()))
            .children(rows)
            .child(div().h(px(24.)))
    }
}
