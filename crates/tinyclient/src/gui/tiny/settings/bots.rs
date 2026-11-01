//! Bots: application tokens an owner or an admin adds to this server. The token is shown once,
//! when the bot is created. A listed bot has no token. Revoke ends it.

use super::kit::{self, cell, table_head, table_row};
use super::page_head;
use crate::client::ask;
use crate::gui::components::{button, button_with_icon, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Page as Facts;
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, SharedString, Window};
use protocol::{Request, Response, TinyBot};

/// What a bot may be granted, in the order the page offers them.
const GRANTS: [(&str, &str); 3] = [
    ("rooms.read", "See rooms and room events"),
    ("rooms.write", "Create, rename, and delete rooms"),
    ("members.read", "See people and presence"),
];

/// The bots read for this Server Settings, and the token from the last create.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub rows: Vec<TinyBot>,
    pub loading: bool,
    pub asked: bool,
    pub error: Option<String>,
    /// Shown once. Cleared when the page is left or another bot is made.
    pub token: Option<String>,
    pub rooms_read: bool,
    pub rooms_write: bool,
    pub members_read: bool,
}

fn scopes_of(draft: &Draft) -> Vec<String> {
    let on = [draft.rooms_read, draft.rooms_write, draft.members_read];
    GRANTS.iter().zip(on).filter(|(_, on)| *on).map(|((scope, _), _)| (*scope).to_owned()).collect()
}

fn grant_on(draft: &Draft, scope: &str) -> bool {
    match scope {
        "rooms.read" => draft.rooms_read,
        "rooms.write" => draft.rooms_write,
        "members.read" => draft.members_read,
        _ => false,
    }
}

fn set_grant(draft: &mut Draft, scope: &str) {
    match scope {
        "rooms.read" => draft.rooms_read = !draft.rooms_read,
        "rooms.write" => draft.rooms_write = !draft.rooms_write,
        "members.read" => draft.members_read = !draft.members_read,
        _ => {}
    }
}

impl Shell {
    pub(in crate::gui) fn tiny_bots_load(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let Some(draft) = self.tiny_state.settings.as_mut().map(|overlay| &mut overlay.bots) else {
            return;
        };
        draft.asked = true;
        draft.loading = true;
        draft.error = None;
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyBots { account }),
            move |this, answer, cx| {
                let Some(draft) = this.tiny_state.settings.as_mut().map(|overlay| &mut overlay.bots) else {
                    return;
                };
                draft.loading = false;
                match answer {
                    Ok(Response::TinyBots { bots }) => draft.rows = bots,
                    Ok(Response::Error { detail, .. }) => draft.error = Some(detail),
                    Ok(_) => draft.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => draft.error = Some(crate::client::trouble_words("tiny", &err)),
                }
                cx.notify();
            },
        );
    }

    pub(in crate::gui) fn tiny_bot_create(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let name = self.tiny_state.settings_fields.bot_name.read(cx).text().to_owned();
        let Some(overlay) = self.tiny_state.settings.as_mut() else {
            return;
        };
        let scopes = scopes_of(&overlay.bots);
        overlay.bots.loading = true;
        overlay.bots.error = None;
        overlay.bots.token = None;
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyBotCreate { account, name, scopes }),
            move |this, answer, cx| {
                let Some(draft) = this.tiny_state.settings.as_mut().map(|overlay| &mut overlay.bots) else {
                    return;
                };
                draft.loading = false;
                match answer {
                    Ok(Response::TinyBotCreated { token, .. }) => {
                        draft.token = Some(token);
                        drop(draft);
                        this.tiny_bots_load(cx);
                    }
                    Ok(Response::Error { detail, .. }) => {
                        draft.error = Some(detail);
                        cx.notify();
                    }
                    Ok(_) => {
                        draft.error = Some(crate::client::OTHER_ANSWER.to_owned());
                        cx.notify();
                    }
                    Err(err) => {
                        draft.error = Some(crate::client::trouble_words("tiny", &err));
                        cx.notify();
                    }
                }
            },
        );
    }

    pub(in crate::gui) fn tiny_bot_revoke(&mut self, bot_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if let Some(draft) = self.tiny_state.settings.as_mut().map(|overlay| &mut overlay.bots) {
            draft.loading = true;
            draft.error = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyBotRevoke { account, bot_id }),
            move |this, answer, cx| {
                let Some(draft) = this.tiny_state.settings.as_mut().map(|overlay| &mut overlay.bots) else {
                    return;
                };
                draft.loading = false;
                match answer {
                    Ok(Response::Ok) => {
                        drop(draft);
                        this.tiny_bots_load(cx);
                    }
                    Ok(Response::Error { detail, .. }) => {
                        draft.error = Some(detail);
                        cx.notify();
                    }
                    Ok(_) => {
                        draft.error = Some(crate::client::OTHER_ANSWER.to_owned());
                        cx.notify();
                    }
                    Err(err) => {
                        draft.error = Some(crate::client::trouble_words("tiny", &err));
                        cx.notify();
                    }
                }
            },
        );
    }

    fn tiny_bot_grant(&mut self, scope: &'static str, cx: &mut Context<Self>) {
        if let Some(draft) = self.tiny_state.settings.as_mut().map(|overlay| &mut overlay.bots) {
            set_grant(draft, scope);
        }
        cx.notify();
    }
}

pub(in crate::gui::tiny) fn bots(shell: &Shell, _facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let Some(draft) = shell.tiny_state.settings.as_ref().map(|overlay| &overlay.bots) else {
        return div().into_any_element();
    };
    let mut page = div().flex().flex_col().gap(px(20.)).child(page_head(
        "Bots",
        "An application token for this server. It is not a member. The token is shown once, when you create the bot.",
    ));
    if let Some(token) = draft.token.clone() {
        let copied = token.clone();
        page = page.child(
            kit::card().child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child("Copy this token now. It will not be shown again."))
                    .child(div().whitespace_normal().type_small().font_family(theme::MONO).text_color(theme::text()).child(token))
                    .child(
                        button("tiny-bots-copy", ButtonKind::Secondary, "Copy token").on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                        })),
                    ),
            ),
        );
    }
    if let Some(error) = draft.error.clone() {
        page = page.child(div().type_small().text_color(theme::danger()).child(error));
    }
    let mut grants = kit::card();
    for (ix, (scope, help)) in GRANTS.into_iter().enumerate() {
        let on = grant_on(draft, scope);
        grants = grants.child(
            kit::switch_row(("tiny-bots-grant", ix), help, scope, on, !draft.loading)
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_bot_grant(scope, cx))),
        );
    }
    let name = kit::input(&shell.tiny_state.settings_fields.bot_name, Some(240.), window, cx);
    let mut create = div().flex().items_center().gap(px(8.)).child(name);
    if !draft.loading {
        create = create.child(
            button_with_icon("tiny-bots-create", ButtonKind::Primary, "icons/bot.svg", "Create bot")
                .on_click(cx.listener(|this, _, _, cx| this.tiny_bot_create(cx))),
        );
    }
    page = page.child(super::section("New bot").child(grants).child(create));
    if draft.loading && draft.rows.is_empty() {
        return page.child(div().type_small().text_color(theme::text_faint()).child("Loading bots…")).into_any_element();
    }
    if draft.rows.is_empty() {
        return page.child(kit::empty("icons/bot.svg", "No bots yet.")).into_any_element();
    }
    let mut table = kit::card().child(table_head(&[("Bot", None), ("Can", Some(220.)), ("", Some(72.))]));
    for (ix, bot) in draft.rows.iter().enumerate() {
        let id = bot.id.clone();
        let can = if bot.scopes.is_empty() { "Identify only".to_owned() } else { bot.scopes.join(", ") };
        let revoke = (!draft.loading).then(|| {
            button(("tiny-bots-revoke", ix), ButtonKind::Ghost, "Revoke").on_click(cx.listener(move |this, _, _, cx| this.tiny_bot_revoke(id.clone(), cx)))
        });
        table = table.child(
            table_row(SharedString::from(format!("tiny-bots-row-{}", bot.id)), false)
                .child(div().flex_1().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(bot.name.clone()))
                .child(cell(220., can, theme::text_soft()))
                .child(div().flex_none().w(px(72.)).flex().justify_end().children(revoke)),
        );
    }
    page.child(table).into_any_element()
}
