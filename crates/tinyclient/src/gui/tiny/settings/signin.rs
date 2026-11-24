//! Sign-in: how people sign in to this server — the provider it sends their browser to (from its
//! `/v1/auth/config`), how many came in through the provider's groups — and who this account is
//! signed in as. "Sign out everywhere" ends every session of this account on the server, this
//! one too (asked first). Between them, this account's own passkeys (`passkeys.rs`).

use super::kit::{card, note};
use super::{page_head, section};
use crate::gui::components::{button, field, glyph, tip, Avatar, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::{Modal, Page as Facts};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Focusable, SharedString, Window};
use protocol::{Request, Response};
use std::sync::atomic::{AtomicBool, Ordering};

static RENAMING_ME: AtomicBool = AtomicBool::new(false);

fn line(label: &'static str, value: impl Into<SharedString>, faint: bool) -> Div {
    div()
        .min_h(px(40.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(16.))
        .border_b_1()
        .border_color(theme::hairline())
        .child(div().w(px(160.)).flex_none().type_ui().text_color(theme::text_soft()).child(label))
        .child(div().flex_1().min_w_0().truncate().type_ui().text_color(if faint { theme::text_faint() } else { theme::text() }).child(value.into()))
}

/// The provider's name in words, or what to say when the server didn't say.
pub(in crate::gui) fn provider_words(provider: &str) -> (String, bool) {
    match provider.trim() {
        "" => ("Not known: this server doesn't say".to_owned(), true),
        name => (name.to_owned(), false),
    }
}

pub(in crate::gui::tiny) fn signin(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let community = facts.community;
    let mut page = div().flex().flex_col().gap(px(28.)).child(page_head("Sign-in & account", "Manage your profile, display name, picture, passkeys, sessions, and account on this server."));
    let (provider, unknown) = provider_words(&community.provider);
    let host = protocol::join_host(&community.origin);
    let groups = community.members.iter().filter(|member| member.joined_via.as_deref() == Some("groups")).count();
    let me = community.member(facts.me);
    let name = me.map(model::name_of).unwrap_or_else(|| "You".to_owned());
    let has_avatar = me.is_some_and(|member| member.avatar.is_some());
    let email = me.map(|member| member.email.clone()).unwrap_or_default();
    let joined = me.and_then(|member| protocol::tiny_unix(&member.created_at)).map(crate::gui::time::date);
    let id = facts.me.to_owned();
    let renaming = RENAMING_ME.load(Ordering::Relaxed);
    let account = facts.account.clone();
    let current_name = name.clone();
    let display_name_row = if renaming {
        let focused = shell.tiny_state.settings_fields.my_name.focus_handle(cx).is_focused(window);
        let save_account = account.clone();
        div()
            .min_h(px(44.))
            .px(px(14.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(div().w(px(160.)).flex_none().type_ui().text_color(theme::text_soft()).child("Display name"))
            .child(field(focused).flex_1().min_w_0().child(shell.tiny_state.settings_fields.my_name.clone()))
            .child(button("tiny-signin-rename-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|_, _, _, cx| {
                RENAMING_ME.store(false, Ordering::Relaxed);
                cx.notify();
            })))
            .child(button("tiny-signin-rename-save", ButtonKind::Primary, "Save").on_click(cx.listener(move |this, _, _, cx| {
                let typed = this.tiny_state.settings_fields.my_name.read(cx).text().to_owned();
                let Some(display_name) = protocol::tiny_name(&typed).map(str::to_owned) else {
                    return;
                };
                RENAMING_ME.store(false, Ordering::Relaxed);
                let req = Request::TinyRename { account: save_account.clone(), display_name };
                this.off_thread(cx, move || crate::client::ask(&req), |this, answer, cx| {
                    if matches!(answer, Ok(Response::Ok)) {
                        this.reload_all(cx);
                    }
                    cx.notify();
                });
                cx.notify();
            })))
    } else {
        line("Display name", name.clone(), false).child(
            button("tiny-signin-rename", ButtonKind::Secondary, "Edit").on_click(cx.listener(move |this, _, window, cx| {
                RENAMING_ME.store(true, Ordering::Relaxed);
                this.tiny_state.settings_fields.my_name.update(cx, |input, cx| input.set_text(&current_name, cx));
                window.focus(&this.tiny_state.settings_fields.my_name.focus_handle(cx), cx);
                cx.notify();
            })),
        )
    };
    page = page.child(
        section("Your profile").child(
            card()
                .child(
                    div()
                        .px(px(14.))
                        .py(px(14.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .border_b_1()
                        .border_color(theme::hairline())
                        .child(
                            div()
                                .id("tiny-signin-avatar")
                                .flex_none()
                                .cursor_pointer()
                                .tooltip(tip("Change your picture", None))
                                .on_click(cx.listener(|this, _, _, cx| this.tiny_pick_avatar(cx)))
                                .child(Avatar::new(&facts.key(facts.me), &name, 48.).image_at(facts.picture(facts.me)).build()),
                        )
                        .child(div().flex_1().min_w_0().flex().flex_col().child(div().type_title().text_color(theme::text()).child(name.clone())).child(
                            div().type_small().text_color(theme::text_soft()).child(if unknown {
                                "Signed in through the server's provider".to_owned()
                            } else {
                                format!("Signed in through {provider}")
                            }),
                        ))
                        .when(facts.may.owner, |row| {
                            row.child(
                                div().flex().items_center().gap(px(4.)).child(glyph("icons/crown.svg", 14., theme::warning())).child(div().type_small().text_color(theme::warning()).child("Owner")),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(button("tiny-signin-avatar-change", ButtonKind::Secondary, "Change picture").on_click(cx.listener(|this, _, _, cx| this.tiny_pick_avatar(cx))))
                                .when(has_avatar, |bar| {
                                    bar.child(button("tiny-signin-avatar-remove", ButtonKind::Ghost, "Remove").on_click(cx.listener(|this, _, _, cx| this.tiny_send_avatar(None, cx))))
                                }),
                        ),
                )
                .child(display_name_row)
                .child(line("Email", if email.is_empty() { "The provider gave none".to_owned() } else { email.clone() }, email.is_empty()))
                .children(joined.map(|joined| line("Member since", joined, false)))
                .child(
                    line("User ID", id.clone(), false)
                        .border_b_0()
                        .child(button("tiny-signin-copy-id", ButtonKind::Ghost, "Copy").on_click(cx.listener(move |this, _, _, cx| this.tiny_copy(id.clone(), "User ID copied", cx)))),
                ),
        ),
    );
    page = page.children(super::passkeys::passkeys(shell, &community.provider, window, cx));
    page = page.child(
        section("Provider")
            .child(
                card().child(line("Signs in through", provider.clone(), unknown)).child(line("Server", if host.is_empty() { "—".to_owned() } else { host.clone() }, host.is_empty())).child(
                    line(
                        "Joined by sign-in group",
                        match groups {
                            0 => "Nobody".to_owned(),
                            1 => "1 person".to_owned(),
                            n => format!("{n} people"),
                        },
                        groups == 0,
                    )
                    .border_b_0(),
                ),
            )
            .child(note(
                "icons/info.svg",
                theme::text_faint(),
                "People sign in in their own browser, at the provider; tinyclient never sees a password. Which provider groups let people in, and with which roles, is set on the server.",
            )),
    );
    page = page.child(
        section("Sessions & account")
            .child(
                card()
                    .p(px(14.))
                    .flex_row()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child("Sign out everywhere")).child(
                            div()
                                .whitespace_normal()
                                .type_small()
                                .text_color(theme::text_soft())
                                .child("Ends every session of yours on this server, on every computer, this one too. You can sign in again when you want back in."),
                        ),
                    )
                    .child(
                        button("tiny-signin-everywhere", ButtonKind::Secondary, "Sign out everywhere").on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::SignOutEverywhere, window, cx))),
                    ),
            )
            .child(
                card()
                    .p(px(14.))
                    .flex_row()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::danger()).child("Delete account")).child(
                            div()
                                .whitespace_normal()
                                .type_small()
                                .text_color(theme::text_soft())
                                .child("Permanently removes your account, sessions, passkeys, recovery codes, and avatar from this server. If you're the last owner, transfer ownership first."),
                        ),
                    )
                    .child(
                        button("tiny-signin-delete-account", ButtonKind::Danger, "Delete account…").on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::DeleteAccount, window, cx))),
                    ),
            ),
    );
    page.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_the_server_did_not_name_is_said_to_be_unknown() {
        assert_eq!(provider_words(" Tinyauth "), ("Tinyauth".to_owned(), false));
        assert!(provider_words("").1);
    }
}
