//! Sign-in: how people sign in to this server — the provider it sends their browser to (from its
//! `/v1/auth/config`), how many came in through the provider's groups — and who this account is
//! signed in as. "Sign out everywhere" ends every session of this account on the server, this
//! one too (asked first).
//!
//! Passkeys: this account's own, read each time the page comes on show and again after anything
//! done to them. Each can be renamed in place or removed (asked first; the last one stays while no
//! recovery code is left). "Add a passkey" opens the server's page for it in the browser, as setup's
//! "Add a passkey for me" does, and "New recovery codes" shows a fresh set once, in a modal. A
//! server older than all this says so to the read, and then the section is not drawn.

use super::kit::{card, empty, input, now, note};
use super::{page_head, section};
use crate::client::ask;
use crate::gui::components::{button, button_disabled, glyph, tip, Avatar, ButtonKind};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::channel::chip;
use crate::gui::tiny::model;
use crate::gui::tiny::{Modal, Page as Facts};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Focusable, SharedString, Window};
use protocol::{Request, Response, TinyPasskey, TinyPasskeys};

/// What chatd answers when the server has no such route (`adapters/tiny/manage.rs`, `OLDER`).
/// chatd is not a dependency of the desktop client, so the words are matched here, once.
const OLDER: &str = "This server can't do that yet. Ask its owner to update tinyserver.";
/// Why the last passkey can't be removed: chatd's `LAST_PASSKEY`, said before the server is asked.
pub(in crate::gui::tiny) const LAST_PASSKEY: &str = "This is your last passkey and you have no recovery codes left, so it stays.";
/// Said once a link to add a passkey has opened in the browser.
const LINK_OPENED: &str = "A page to add a passkey opened in your browser. When you have added one, come back and check again.";

/// The read was refused because the server is older than a member's own passkeys.
fn older(detail: &str) -> bool {
    detail.trim() == OLDER
}

/// Removing this one is refused before it is asked: it is the only passkey and no recovery code
/// is left to sign in with instead.
fn keeps_last(passkeys: &TinyPasskeys) -> bool {
    passkeys.passkeys.len() <= 1 && passkeys.recovery_codes_left == 0
}

/// "Made 3 Oct 2026 · Last used 2 days ago", as Members has Joined and Last seen.
fn made_and_used(passkey: &TinyPasskey, now: i64) -> String {
    let made = protocol::tiny_unix(&passkey.created_at).map(crate::gui::time::date).unwrap_or_else(|| "—".to_owned());
    let used = passkey.last_used_at.as_deref().and_then(protocol::tiny_unix).map(|at| model::relative(now, at)).unwrap_or_else(|| "never".to_owned());
    format!("Made {made} · Last used {used}")
}

fn codes_left(n: u32) -> String {
    match n {
        1 => "1 recovery code left.".to_owned(),
        n => format!("{n} recovery codes left."),
    }
}

/// A passkey's name, or what to call one the server has no name for.
pub(in crate::gui::tiny) fn passkey_name(label: &str) -> String {
    match label.trim() {
        "" => "Passkey".to_owned(),
        name => name.to_owned(),
    }
}

/// Where a link to add a passkey was asked from, so what came of it is said there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui::tiny) enum LinkFrom {
    /// Setup's "Add a passkey for me".
    Setup,
    /// Sign-in's "Add a passkey".
    SignIn,
}

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
    let mut page = div().flex().flex_col().gap(px(28.)).child(page_head("Sign-in", "How people sign in to this server, and who you are signed in as."));
    let (provider, unknown) = provider_words(&community.provider);
    let host = protocol::join_host(&community.origin);
    let groups = community.members.iter().filter(|member| member.joined_via.as_deref() == Some("groups")).count();
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
    let me = community.member(facts.me);
    let name = me.map(model::name_of).unwrap_or_else(|| "You".to_owned());
    let email = me.map(|member| member.email.clone()).unwrap_or_default();
    let joined = me.and_then(|member| protocol::tiny_unix(&member.created_at)).map(crate::gui::time::date);
    let id = facts.me.to_owned();
    page = page.child(
        section("You").child(
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
                                .child(Avatar::new(&facts.key(facts.me), &name, 40.).image_at(facts.picture(facts.me)).build()),
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
                        }),
                )
                .child(line("Email", if email.is_empty() { "The provider gave none".to_owned() } else { email.clone() }, email.is_empty()))
                .children(joined.map(|joined| line("Member since", joined, false)))
                .child(
                    line("User ID", id.clone(), false)
                        .border_b_0()
                        .child(button("tiny-signin-copy-id", ButtonKind::Ghost, "Copy").on_click(cx.listener(move |this, _, _, cx| this.tiny_copy(id.clone(), "User ID copied", cx)))),
                ),
        ),
    );
    page = page.children(passkeys(shell, &community.provider, window, cx));
    page = page.child(
        section("Sessions").child(
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
                            .child("Ends every session of yours on this server, on every computer, this one too. You can sign in again with your provider."),
                    ),
                )
                .child(
                    button("tiny-signin-everywhere", ButtonKind::Danger, "Sign out everywhere").on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::SignOutEverywhere, window, cx))),
                ),
        ),
    );
    page.into_any_element()
}

/// The Passkeys section, between Provider and Sessions; `None` until the first read answers, and
/// on a server older than a member's own passkeys.
fn passkeys(shell: &Shell, provider: &str, window: &mut Window, cx: &mut Context<Shell>) -> Option<Div> {
    let state = &shell.tiny_state;
    if state.passkeys_older || (state.passkeys.is_none() && state.passkeys_error.is_none()) {
        return None;
    }
    let busy = state.passkeys_busy;
    let mut section = section("Passkeys");
    let Some(list) = state.passkeys.as_ref() else {
        // The first read failed: what it said, and a way to ask again.
        let detail = state.passkeys_error.clone().unwrap_or_default();
        return Some(section.child(note("icons/circle-alert.svg", theme::danger(), detail)).child(
            div().flex().child(button("tiny-signin-passkeys-retry", ButtonKind::Secondary, "Try again").on_click(cx.listener(|this, _, _, cx| this.tiny_passkeys_read(cx)))),
        ));
    };
    if list.passkeys.is_empty() {
        let provider = match provider.trim() {
            "" => "the sign-in provider",
            name => name,
        };
        section = section.child(empty("icons/key.svg", format!("No passkeys yet. Add one to sign in without {provider}.")));
    } else {
        let now = now();
        let mut rows = card();
        for (ix, passkey) in list.passkeys.iter().enumerate() {
            rows = rows.child(passkey_row(shell, list, passkey, ix, now, window, cx));
        }
        section = section.child(rows);
    }
    if let Some(detail) = &state.passkeys_error {
        section = section.child(note("icons/circle-alert.svg", theme::danger(), detail.clone()));
    }
    let (add, codes) = if busy {
        (button_disabled("Add a passkey").into_any_element(), button_disabled("New recovery codes").into_any_element())
    } else {
        (
            button("tiny-signin-passkey-add", ButtonKind::Primary, "Add a passkey").on_click(cx.listener(|this, _, _, cx| this.tiny_passkey_link(LinkFrom::SignIn, cx))).into_any_element(),
            button("tiny-signin-codes", ButtonKind::Secondary, "New recovery codes").on_click(cx.listener(|this, _, _, cx| this.tiny_recovery_codes(cx))).into_any_element(),
        )
    };
    let left = if list.recovery_codes_left == 0 { theme::warning() } else { theme::text_soft() };
    section = section.child(
        div().flex().items_center().gap(px(8.)).child(add).child(codes).child(div().flex_1().min_w_0().text_right().type_small().text_color(left).child(codes_left(list.recovery_codes_left))),
    );
    if let Some(words) = &state.passkeys_note {
        section = section.child(
            div().flex().flex_col().gap(px(8.)).child(note("icons/info.svg", theme::text_faint(), words.clone())).child(
                div().flex().child(button("tiny-signin-passkeys-check", ButtonKind::Secondary, "Check again").on_click(cx.listener(|this, _, _, cx| {
                    this.tiny_state.passkeys_note = None;
                    this.tiny_passkeys_read(cx);
                }))),
            ),
        );
    }
    Some(section)
}

/// One passkey: its name (or the field to rename it in), when it was made and last used, whether
/// it is synced or flagged, and Rename and Remove.
fn passkey_row(shell: &Shell, list: &TinyPasskeys, passkey: &TinyPasskey, ix: usize, now: i64, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    let busy = shell.tiny_state.passkeys_busy;
    let editing = shell.tiny_state.renaming.as_ref().filter(|(id, _)| *id == passkey.id).map(|(_, field)| field.clone());
    let name = passkey_name(&passkey.label);
    let title = match &editing {
        Some(field) => input(field, None, window, cx),
        None => div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(name.clone()))
            .when(passkey.synced, |line| line.child(chip("Synced", theme::text_soft(), theme::hover())))
            .when(passkey.flagged, |line| line.child(chip("Flagged", theme::warning(), theme::warning_soft()))),
    };
    let mut actions = div().flex_none().flex().items_center().gap(px(4.));
    if editing.is_some() {
        let save = if busy {
            button_disabled("Save").into_any_element()
        } else {
            button("tiny-signin-passkey-save", ButtonKind::Primary, "Save").on_click(cx.listener(|this, _, _, cx| this.tiny_passkey_rename_save(cx))).into_any_element()
        };
        actions = actions.child(button("tiny-signin-passkey-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| this.tiny_passkey_rename_cancel(cx)))).child(save);
    } else if busy {
        actions = actions.child(button_disabled("Rename")).child(button_disabled("Remove"));
    } else {
        let (id, label) = (passkey.id.clone(), passkey.label.clone());
        actions = actions.child(button(("tiny-signin-passkey-rename", ix), ButtonKind::Ghost, "Rename").on_click(cx.listener(move |this, _, window, cx| this.tiny_passkey_rename_start(id.clone(), &label, window, cx))));
        actions = actions.child(if keeps_last(list) {
            button_disabled("Remove").id(("tiny-signin-passkey-kept", ix)).tooltip(tip(LAST_PASSKEY, None)).into_any_element()
        } else {
            let (id, label) = (passkey.id.clone(), name.clone());
            button(("tiny-signin-passkey-remove", ix), ButtonKind::Ghost, "Remove")
                .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::RemovePasskey { id: id.clone(), label: label.clone() }, window, cx)))
                .into_any_element()
        });
    }
    div()
        .px(px(14.))
        .py(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .when(ix > 0, |row| row.border_t_1().border_color(theme::hairline()))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(title).child(div().truncate().type_small().text_color(theme::text_faint()).child(made_and_used(passkey, now))))
        .child(actions)
}

/// Words for an answer that was not what was asked for; `None` when it was.
fn failed(answer: std::io::Result<Response>) -> Option<String> {
    match answer {
        Ok(Response::Ok | Response::TinyPasskeys { .. } | Response::TinyRecoveryCodes { .. }) => None,
        Ok(Response::Error { detail, .. }) => Some(detail),
        Ok(_) => Some(crate::client::OTHER_ANSWER.to_owned()),
        Err(err) => Some(crate::client::trouble_words("tiny", &err)),
    }
}

impl Shell {
    /// The page comes on show: what was being typed or said goes, and the passkeys are read.
    pub(in crate::gui::tiny) fn tiny_signin_enter(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.renaming = None;
        self.tiny_state.passkeys_note = None;
        self.tiny_passkeys_read(cx);
    }

    /// This account's own passkeys. An answer for an account no longer on show is dropped.
    pub(in crate::gui::tiny) fn tiny_passkeys_read(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let asked = account.clone();
        self.off_thread(cx, move || ask(&Request::TinyPasskeysRead { account: asked }), move |this, answer, cx| {
            if this.tiny_state.account.as_ref() != Some(&account) {
                return;
            }
            let state = &mut this.tiny_state;
            match answer {
                Ok(Response::TinyPasskeys { passkeys }) => {
                    state.passkeys = Some(passkeys);
                    state.passkeys_error = None;
                    state.passkeys_older = false;
                }
                Ok(Response::Error { detail, .. }) if older(&detail) => {
                    state.passkeys = None;
                    state.passkeys_error = None;
                    state.passkeys_older = true;
                }
                other => state.passkeys_error = Some(failed(other).unwrap_or_else(|| crate::client::OTHER_ANSWER.to_owned())),
            }
            cx.notify();
        });
    }

    /// Asks the server for a link that adds a passkey for this account. chatd opens it in the
    /// system browser; what came of it is said where it was asked from. From Sign-in the passkeys
    /// are read again after.
    pub(in crate::gui::tiny) fn tiny_passkey_link(&mut self, from: LinkFrom, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let state = &mut self.tiny_state;
        match from {
            LinkFrom::Setup if state.setup.linking => return,
            LinkFrom::Setup => {
                state.setup.linking = true;
                state.setup.link_note = None;
            }
            LinkFrom::SignIn if state.passkeys_busy => return,
            LinkFrom::SignIn => {
                state.passkeys_busy = true;
                state.passkeys_note = None;
                state.passkeys_error = None;
            }
        }
        cx.notify();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyLink { account }), move |this, answer, cx| {
            let words = failed(answer).unwrap_or_else(|| LINK_OPENED.to_owned());
            let state = &mut this.tiny_state;
            match from {
                LinkFrom::Setup => {
                    state.setup.linking = false;
                    state.setup.link_note = Some(words);
                }
                LinkFrom::SignIn => {
                    state.passkeys_busy = false;
                    state.passkeys_note = Some(words);
                    this.tiny_passkeys_read(cx);
                }
            }
            cx.notify();
        });
    }

    /// Rename: the name becomes a field, focused, with what it is called now. Return saves it,
    /// Escape leaves it as it was.
    fn tiny_passkey_rename_start(&mut self, id: String, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextInput::new("Passkey name", cx));
        field.update(cx, |field, cx| field.set_text(label, cx));
        cx.subscribe(&field, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                this.tiny_passkey_rename_save(cx);
            }
        })
        .detach();
        cx.subscribe(&field, |this, _, event: &InputKey, cx| {
            if *event == InputKey::Escape {
                this.tiny_passkey_rename_cancel(cx);
            }
        })
        .detach();
        window.focus(&field.focus_handle(cx), cx);
        self.tiny_state.passkeys_error = None;
        self.tiny_state.renaming = Some((id, field));
        cx.notify();
    }

    fn tiny_passkey_rename_cancel(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.renaming = None;
        self.tiny_state.passkeys_error = None;
        cx.notify();
    }

    /// The new name to the server. Nothing typed, or the name it has, is no change.
    fn tiny_passkey_rename_save(&mut self, cx: &mut Context<Self>) {
        let (Some(account), Some((id, field))) = (self.tiny_state.account.clone(), self.tiny_state.renaming.clone()) else {
            return;
        };
        if self.tiny_state.passkeys_busy {
            return;
        }
        let label = field.read(cx).text().trim().to_owned();
        let now = self.tiny_state.passkeys.as_ref().and_then(|list| list.passkeys.iter().find(|passkey| passkey.id == id)).map(|passkey| passkey.label.trim().to_owned());
        if label.is_empty() || now.as_deref() == Some(label.as_str()) {
            self.tiny_passkey_rename_cancel(cx);
            return;
        }
        self.tiny_state.passkeys_busy = true;
        self.tiny_state.passkeys_error = None;
        cx.notify();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyRename { account, id, label }), |this, answer, cx| {
            this.tiny_state.passkeys_busy = false;
            // A refusal stays said: a read now would clear it.
            match failed(answer) {
                None => {
                    this.tiny_state.renaming = None;
                    this.tiny_passkeys_read(cx);
                }
                Some(detail) => this.tiny_state.passkeys_error = Some(detail),
            }
            cx.notify();
        });
    }

    /// From the modal: the passkey goes. A refusal is said in the modal, which stays.
    pub(in crate::gui::tiny) fn tiny_passkey_remove(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if self.tiny_state.passkeys_busy {
            return;
        }
        self.tiny_state.passkeys_busy = true;
        self.tiny_state.modal_error = None;
        cx.notify();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyRemove { account, id }), |this, answer, cx| {
            this.tiny_state.passkeys_busy = false;
            match failed(answer) {
                None => this.close_modal(cx),
                Some(detail) => this.tiny_state.modal_error = Some(detail),
            }
            this.tiny_passkeys_read(cx);
            cx.notify();
        });
    }

    /// A fresh set of recovery codes, which replaces the old ones, shown once in a modal. They are
    /// kept only in it.
    fn tiny_recovery_codes(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if self.tiny_state.passkeys_busy {
            return;
        }
        self.tiny_state.passkeys_busy = true;
        self.tiny_state.passkeys_error = None;
        self.tiny_state.passkeys_note = None;
        cx.notify();
        self.off_thread(cx, move || ask(&Request::TinyRecoveryCodes { account }), |this, answer, cx| {
            this.tiny_state.passkeys_busy = false;
            match answer {
                Ok(Response::TinyRecoveryCodes { codes }) => {
                    this.tiny_show_codes(codes, cx);
                    this.tiny_passkeys_read(cx);
                }
                other => this.tiny_state.passkeys_error = Some(failed(other).unwrap_or_else(|| crate::client::OTHER_ANSWER.to_owned())),
            }
            cx.notify();
        });
    }

    /// The codes in their modal, the only place they are kept.
    pub(in crate::gui::tiny) fn tiny_show_codes(&mut self, codes: Vec<String>, cx: &mut Context<Self>) {
        self.tiny_state.float = None;
        self.tiny_state.modal_error = None;
        self.tiny_state.copied = false;
        self.tiny_state.modal = Some(Modal::RecoveryCodes(codes));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_the_server_did_not_name_is_said_to_be_unknown() {
        assert_eq!(provider_words(" Tinyauth "), ("Tinyauth".to_owned(), false));
        assert!(provider_words("").1);
    }

    fn passkey(id: &str) -> TinyPasskey {
        TinyPasskey { id: id.into(), label: String::new(), created_at: "2026-10-01T09:00:00Z".into(), last_used_at: None, synced: false, flagged: false }
    }

    #[test]
    fn the_last_passkey_stays_only_while_no_recovery_code_is_left() {
        let one = TinyPasskeys { passkeys: vec![passkey("a")], recovery_codes_left: 0 };
        assert!(keeps_last(&one));
        assert!(!keeps_last(&TinyPasskeys { recovery_codes_left: 1, ..one.clone() }));
        assert!(!keeps_last(&TinyPasskeys { passkeys: vec![passkey("a"), passkey("b")], recovery_codes_left: 0 }));
    }

    #[test]
    fn a_passkey_says_when_it_was_made_and_last_used() {
        let mut key = passkey("a");
        let now = protocol::tiny_unix("2026-11-03T09:00:00Z").unwrap();
        assert!(made_and_used(&key, now).ends_with("· Last used never"), "{}", made_and_used(&key, now));
        key.last_used_at = Some("2026-11-01T09:00:00Z".into());
        assert!(made_and_used(&key, now).ends_with("· Last used 2 days ago"), "{}", made_and_used(&key, now));
        assert!(made_and_used(&key, now).starts_with("Made "));
        assert_eq!(passkey_name("  "), "Passkey");
        assert_eq!((codes_left(1), codes_left(7)), ("1 recovery code left.".to_owned(), "7 recovery codes left.".to_owned()));
    }

    #[test]
    fn only_the_older_server_words_hide_the_section() {
        assert!(older(OLDER));
        assert!(!older("That passkey is gone."));
    }
}
