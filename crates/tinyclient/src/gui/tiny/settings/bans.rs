//! Bans: who is kept out, why, by whom, since when and until when, with a search. Unban asks
//! first. A ban holds every identity the person signed in with, so neither an invite, a sign-in
//! group nor signing in again lets them back until it ends.

use super::kit::{self, cell, table_head, table_row};
use super::page_head;
use crate::gui::components::{button, Avatar, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::{Modal, Page as Facts};
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString, Window};
use protocol::{Request, TinyBan};

/// The bans matching `query` by name, email or reason, newest first.
pub(in crate::gui) fn matching<'a>(bans: &'a [TinyBan], query: &str) -> Vec<&'a TinyBan> {
    let query = query.trim().to_lowercase();
    let mut rows: Vec<&TinyBan> = bans.iter().filter(|ban| query.is_empty() || [&ban.display_name, &ban.email, &ban.reason].iter().any(|text| text.to_lowercase().contains(&query))).collect();
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    rows
}

/// A banned person's name: theirs, else their email, else "Someone".
pub(in crate::gui) fn banned_name(ban: &TinyBan) -> String {
    [&ban.display_name, &ban.email].into_iter().find(|text| !text.trim().is_empty()).cloned().unwrap_or_else(|| "Someone".to_owned())
}

impl Shell {
    pub(in crate::gui) fn tiny_unban(&mut self, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.act(Request::TinyUnban { account, user_id }, cx);
    }
}

pub(in crate::gui::tiny) fn bans(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let community = facts.community;
    let fields = &shell.tiny_state.settings_fields;
    let query = fields.ban_search.read(cx).text().to_owned();
    let rows = matching(&community.bans, &query);
    let mut page = div()
        .flex()
        .flex_col()
        .gap(px(20.))
        .child(page_head("Bans", "People kept out of this server. A ban holds whatever they sign in with, so an invite, a sign-in group or a new sign-in won't let them back."));
    page = page.child(div().flex().items_center().gap(px(8.)).child(kit::search(&fields.ban_search, 260., window, cx)).child(div().flex_1()).child(
        div().type_small().text_color(theme::text_faint()).child(match community.bans.len() {
            1 => "1 ban".to_owned(),
            n => format!("{n} bans"),
        }),
    ));
    if community.bans.is_empty() {
        return page.child(kit::empty("icons/ban.svg", "Nobody is banned.")).into_any_element();
    }
    let mut table = kit::card().child(table_head(&[("Person", None), ("Reason", Some(160.)), ("Banned", Some(116.)), ("Until", Some(76.)), ("", Some(64.))]));
    let now = kit::now();
    for (ix, ban) in rows.iter().enumerate() {
        let name = banned_name(ban);
        let when = protocol::tiny_unix(&ban.created_at).map(crate::gui::time::date).unwrap_or_default();
        let until = match ban.expires_at.as_deref().and_then(protocol::tiny_unix) {
            Some(at) => model::relative(now, at),
            None => "For good".to_owned(),
        };
        let by =
            if ban.banned_by_name.trim().is_empty() { ban.banned_by.as_deref().map(|id| model::name_by_id(community, id)).unwrap_or_else(|| "Server".to_owned()) } else { ban.banned_by_name.clone() };
        let id = ban.user_id.clone();
        let key = facts.key(&ban.user_id);
        table =
            table.child(
                table_row(SharedString::from(format!("tiny-bans-row-{}", ban.user_id)), false)
                    .child(
                        div().flex_1().min_w_0().flex().items_center().gap(px(10.)).child(Avatar::new(&key, &name, 32.).image_at(facts.picture(&ban.user_id)).build()).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(name))
                                .when(!ban.email.is_empty() && facts.may.see_email, |column| {
                                    column.child(div().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(ban.email.clone()))
                                }),
                        ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(160.))
                            .whitespace_normal()
                            .type_small()
                            .text_color(if ban.reason.is_empty() { theme::text_faint() } else { theme::text_soft() })
                            .child(if ban.reason.is_empty() { "No reason given".to_owned() } else { ban.reason.clone() }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(116.))
                            .flex()
                            .flex_col()
                            .child(div().truncate().type_small().text_color(theme::text_soft()).child(when))
                            .child(div().truncate().type_small().text_color(theme::text_faint()).child(format!("by {by}"))),
                    )
                    .child(cell(76., until, if ban.expires_at.is_none() { theme::text() } else { theme::text_soft() }))
                    .child(div().flex_none().w(px(64.)).flex().justify_end().child(
                        button(("tiny-bans-unban", ix), ButtonKind::Ghost, "Unban").on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::Unban(id.clone()), window, cx))),
                    )),
            );
    }
    if rows.is_empty() {
        table = table.child(div().py(px(28.)).flex().justify_center().type_ui().text_color(theme::text_soft()).child(format!("No ban matches “{}”.", query.trim())));
    }
    page.child(table).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bans_are_searched_by_name_email_or_reason_newest_first() {
        let ban = |id: &str, name: &str, reason: &str, at: &str| TinyBan {
            user_id: id.into(),
            display_name: name.into(),
            email: format!("{id}@x.example"),
            reason: reason.into(),
            created_at: at.into(),
            ..TinyBan::default()
        };
        let bans = [ban("a", "Rook", "Harassment", "2026-01-01T00:00:00Z"), ban("b", "", "Spam", "2026-02-01T00:00:00Z")];
        let ids = |rows: Vec<&TinyBan>| rows.into_iter().map(|ban| ban.user_id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(matching(&bans, "")), ["b", "a"]);
        assert_eq!(ids(matching(&bans, "spam")), ["b"]);
        assert_eq!(ids(matching(&bans, "a@x")), ["a"]);
        assert_eq!(banned_name(&bans[1]), "b@x.example");
    }
}
