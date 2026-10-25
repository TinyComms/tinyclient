//! Settings › Accounts, each Tiny server's own rows: the name, the password, and signing out of
//! every device. They show only while that account is connected. The account is named by its
//! host, so the name shown here is read from the server's member list. A typed password lives
//! only in its field and is cleared when the question closes.

use super::controls::{rule, row};
use super::{Confirm, SettingsWindow};
use crate::gui::components::{button, button_disabled, field, modal, ButtonKind};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Entity, Focusable, MouseButton, Subscription, Window};
use protocol::{AccountKey, AccountStatus, Link, Network, Request, Response, TinySecret};
use std::collections::HashMap;

const MISMATCH: &str = "The new passwords don't match.";
const SIZE: &str = "A new password is 12 to 128 characters.";
const NO_CURRENT: &str = "Type your current password.";
const NAME_SIZE: &str = "A name is 1 to 80 characters.";

pub(super) struct TinyAccount {
    name: Entity<TextInput>,
    /// The account whose name field is open.
    naming: Option<AccountKey>,
    /// Each connected server's display name for this account, once the server said. `None`
    /// while it is being asked, empty when it did not say. Asked again when Accounts is opened.
    names: HashMap<AccountKey, Option<String>>,
    current: Entity<TextInput>,
    new: Entity<TextInput>,
    again: Entity<TextInput>,
    busy: bool,
    /// Under the card: a failed rename or sign-out, or what just changed.
    line: Option<(String, bool)>,
    /// In the password question.
    problem: Option<String>,
}

impl TinyAccount {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> (TinyAccount, Vec<Subscription>) {
        let name = cx.new(|cx| TextInput::new("Name", cx));
        let current = cx.new(|cx| TextInput::new("", cx));
        let new = cx.new(|cx| TextInput::new("", cx));
        let again = cx.new(|cx| TextInput::new("", cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Submit => this.save_tiny_name(cx),
                InputEvent::Changed => {
                    this.tiny.line = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&name, window, |this, _, event: &InputKey, _, cx| {
                if *event == InputKey::Escape {
                    this.tiny.naming = None;
                    cx.notify();
                }
            }),
        ];
        for input in [&current, &new, &again] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Submit => this.send_tiny_password(cx),
                InputEvent::Changed => {
                    this.tiny.problem = None;
                    cx.notify();
                }
            }));
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event: &InputKey, _, cx| {
                if *event == InputKey::Escape {
                    this.close_modal(cx);
                }
            }));
        }
        let account = TinyAccount { name, naming: None, names: HashMap::new(), current, new, again, busy: false, line: None, problem: None };
        (account, subscriptions)
    }

    /// Forgets the names read, so opening Accounts asks each server again.
    pub(super) fn forget_names(&mut self) {
        self.names.clear();
    }

    /// Drops what was typed into the password fields. It lives nowhere else.
    pub(super) fn forget_typed(&mut self, cx: &mut Context<SettingsWindow>) {
        for input in [&self.current, &self.new, &self.again] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.problem = None;
    }
}

/// What is wrong with the three fields, before anything is sent. The server counts bytes.
pub(super) fn password_problem(current: &str, new: &str, again: &str) -> Option<&'static str> {
    if current.is_empty() {
        Some(NO_CURRENT)
    } else if !protocol::tiny_password_fits(new) {
        Some(SIZE)
    } else if new != again {
        Some(MISMATCH)
    } else {
        None
    }
}

impl SettingsWindow {
    /// One Tiny account's rows under its account row. `ix` keeps the ids apart.
    pub(super) fn tiny_rows(&mut self, account: &AccountStatus, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let busy = self.tiny.busy;
        let known = self.tiny.names.get(&account.key).cloned().flatten().filter(|name| !name.is_empty());
        let name_row = if self.tiny.naming.as_ref() == Some(&account.key) {
            let focused = self.tiny.name.focus_handle(cx).is_focused(window);
            let save = if busy {
                button_disabled("Save").into_any_element()
            } else {
                button(("tiny-name-save", ix), ButtonKind::Primary, "Save").on_click(cx.listener(|this, _, _, cx| this.save_tiny_name(cx))).into_any_element()
            };
            row(
                "Name",
                None,
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(field(focused).w(px(200.)).child(self.tiny.name.clone()))
                    .child(button(("tiny-name-cancel", ix), ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.tiny.naming = None;
                        cx.notify();
                    })))
                    .child(save),
            )
        } else {
            let key = account.key.clone();
            let shown = known.clone().unwrap_or_default();
            row(
                "Name",
                Some(known.unwrap_or_else(|| "What people on this server see.".to_owned()).into()),
                button(("tiny-name-edit", ix), ButtonKind::Secondary, "Change…").on_click(cx.listener(move |this, _, window, cx| {
                    this.tiny.naming = Some(key.clone());
                    this.tiny.line = None;
                    this.tiny.name.update(cx, |input, cx| input.set_text(&shown, cx));
                    window.focus(&this.tiny.name.focus_handle(cx), cx);
                    cx.notify();
                })),
            )
        };
        let key = account.key.clone();
        let everywhere = Box::new(account.clone());
        div()
            .flex()
            .flex_col()
            .child(rule())
            .child(name_row)
            .child(rule())
            .child(row(
                "Password",
                Some("Changing it signs out your other devices.".into()),
                button(("tiny-password", ix), ButtonKind::Secondary, "Change…").on_click(cx.listener(move |this, _, window, cx| {
                    this.tiny.forget_typed(cx);
                    this.tiny.line = None;
                    this.confirm = Some(Confirm::TinyPassword(key.clone()));
                    window.focus(&this.tiny.current.focus_handle(cx), cx);
                    cx.notify();
                })),
            ))
            .child(rule())
            .child(row(
                "Sign out everywhere",
                Some("Every device signed in as you, this one too.".into()),
                button(("tiny-everywhere", ix), ButtonKind::Ghost, "Sign out everywhere…").on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny.line = None;
                    this.confirm = Some(Confirm::TinyEverywhere(everywhere.clone()));
                    cx.notify();
                })),
            ))
    }

    /// Asks each connected Tiny server, once, for the name this account goes by there.
    pub(super) fn read_tiny_names(&mut self, cx: &mut Context<Self>) {
        let connected = self.tiny_accounts(cx);
        self.tiny.names.retain(|key, _| connected.contains(key));
        for account in connected {
            if self.tiny.names.contains_key(&account) {
                continue;
            }
            self.tiny.names.insert(account.clone(), None);
            self.read_tiny_name(account, cx);
        }
    }

    fn read_tiny_name(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        let request = Request::TinyCommunity { account: account.clone() };
        self.off_thread(cx, move || crate::client::ask(&request), move |this, answer, cx| {
            let mine = match answer {
                Ok(Response::TinyCommunity { community }) => {
                    community.members.into_iter().find(|member| member.user_id == account.account).map(|member| member.display_name)
                }
                _ => None,
            };
            // An empty name is kept, so a server that did not answer is not asked on every redraw.
            this.tiny.names.insert(account, Some(mine.unwrap_or_default()));
            cx.notify();
        });
    }

    /// Words under the Tiny card, when there are any.
    pub(super) fn tiny_line(&self) -> Option<Div> {
        self.tiny.line.as_ref().map(|(words, bad)| {
            let color = if *bad { theme::danger() } else { theme::text_soft() };
            div().pt(px(6.)).px(px(12.)).type_small().text_color(color).child(words.clone())
        })
    }

    /// The connected Tiny accounts, in the shell's order.
    pub(super) fn tiny_accounts(&self, cx: &Context<Self>) -> Vec<AccountKey> {
        let Some(shell) = self.shell.upgrade() else {
            return Vec::new();
        };
        let shell = shell.read(cx);
        let Some(snapshot) = shell.snapshot.as_ref() else {
            return Vec::new();
        };
        snapshot
            .accounts
            .iter()
            .filter(|status| status.key.network == Network::Tiny && status.link == Link::Connected)
            .map(|status| status.key.clone())
            .collect()
    }

    fn save_tiny_name(&mut self, cx: &mut Context<Self>) {
        if self.tiny.busy {
            return;
        }
        let Some(account) = self.tiny.naming.clone() else {
            return;
        };
        let typed = self.tiny.name.read(cx).text().to_owned();
        let Some(display_name) = protocol::tiny_name(&typed).map(str::to_owned) else {
            self.tiny.line = Some((NAME_SIZE.to_owned(), true));
            cx.notify();
            return;
        };
        self.tiny.busy = true;
        cx.notify();
        let request = Request::TinyRename { account: account.clone(), display_name: display_name.clone() };
        self.off_thread(cx, move || crate::client::ask(&request), move |this, answer, cx| {
            this.tiny.busy = false;
            match answer {
                Ok(Response::Ok) => {
                    this.tiny.naming = None;
                    this.tiny.names.insert(account, Some(display_name));
                    this.tiny.line = Some(("Your name is changed.".to_owned(), false));
                }
                other => this.tiny.line = Some((failure(other), true)),
            }
            cx.notify();
        });
    }

    fn send_tiny_password(&mut self, cx: &mut Context<Self>) {
        let Some(Confirm::TinyPassword(account)) = self.confirm.clone() else {
            return;
        };
        if self.tiny.busy {
            return;
        }
        let current = self.tiny.current.read(cx).text().to_owned();
        let new = self.tiny.new.read(cx).text().to_owned();
        let again = self.tiny.again.read(cx).text().to_owned();
        if let Some(problem) = password_problem(&current, &new, &again) {
            self.tiny.problem = Some(problem.to_owned());
            cx.notify();
            return;
        }
        self.tiny.busy = true;
        self.tiny.problem = None;
        cx.notify();
        let request = Request::TinyPassword { account, current_password: TinySecret(current), new_password: TinySecret(new) };
        self.off_thread(cx, move || crate::client::ask(&request), |this, answer, cx| {
            this.tiny.busy = false;
            match answer {
                Ok(Response::Ok) => {
                    this.close_modal(cx);
                    this.tiny.line = Some(("Your password is changed. Your other devices are signed out.".to_owned(), false));
                }
                // A wrong current password leaves the question open, so it can be typed again.
                other => this.tiny.problem = Some(failure(other)),
            }
            cx.notify();
        });
    }

    fn send_tiny_everywhere(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        self.confirm = None;
        self.tiny.busy = true;
        cx.notify();
        let request = Request::TinySignOutEverywhere { account };
        self.off_thread(cx, move || crate::client::ask(&request), |this, answer, cx| {
            this.tiny.busy = false;
            if !matches!(answer, Ok(Response::Status { .. })) {
                this.tiny.line = Some((failure(answer), true));
            }
            let _ = this.shell.update(cx, |shell, cx| {
                shell.load_status(cx);
                shell.reload_all(cx);
            });
            cx.notify();
        });
    }

    /// The password question: three fields that show bullets, and what is wrong with them.
    pub(super) fn tiny_password_modal(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let change = if self.tiny.busy {
            button_disabled("Change password").into_any_element()
        } else {
            button("tiny-password-confirm", ButtonKind::Primary, "Change password")
                .on_click(cx.listener(|this, _, _, cx| this.send_tiny_password(cx)))
                .into_any_element()
        };
        let panel = modal("Change password")
            .child(div().mt(px(8.)).type_ui().text_color(theme::text_soft()).child("12 to 128 characters. Your other devices are signed out. This one stays signed in."))
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(secret(&self.tiny.current, "Current password", window, cx))
                    .child(secret(&self.tiny.new, "New password", window, cx))
                    .child(secret(&self.tiny.again, "New password again", window, cx)),
            )
            .children(self.tiny.problem.as_ref().map(|problem| div().mt(px(8.)).type_small().text_color(theme::danger()).child(problem.clone())))
            .child(
                div()
                    .mt(px(20.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("tiny-password-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
                    .child(change),
            );
        super::modal_layer(panel, cx)
    }

    pub(super) fn tiny_everywhere_modal(&self, account: &AccountStatus, cx: &mut Context<Self>) -> Div {
        let key = account.key.clone();
        // The account is named by its host.
        let host = if account.name.is_empty() { "this Tiny server".to_owned() } else { account.name.clone() };
        super::modal_panel(
            "Sign out everywhere?",
            vec![
                format!("Every device signed in to {host} as you is signed out, this one too."),
                "Sign in again with your password.".to_owned(),
            ],
            button("tiny-everywhere-confirm", ButtonKind::Danger, "Sign out everywhere")
                .on_click(cx.listener(move |this, _, _, cx| this.send_tiny_everywhere(key.clone(), cx))),
            cx,
        )
    }
}

fn failure(answer: std::io::Result<Response>) -> String {
    match answer {
        Ok(Response::Error { detail, .. }) => detail,
        Ok(_) => crate::client::OTHER_ANSWER.to_owned(),
        Err(err) => crate::client::trouble_words(&err),
    }
}

/// A field that draws bullets. The real one is underneath with its letters transparent: it takes
/// the keys, the paste and the input method.
fn secret(input: &Entity<TextInput>, placeholder: &'static str, window: &Window, cx: &mut Context<SettingsWindow>) -> Div {
    let handle = input.focus_handle(cx);
    let focused = handle.is_focused(window);
    let count = input.read(cx).text().chars().count();
    let cover = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .rounded(px(theme::R_CONTROL - 1.))
        .bg(theme::field())
        .px(px(8.))
        .flex()
        .items_center()
        .overflow_hidden()
        .map(|cover| {
            if count == 0 && !focused {
                cover.child(div().text_color(theme::text_soft()).child(placeholder))
            } else {
                cover.child(div().flex_none().child("\u{2022}".repeat(count)))
            }
        })
        .children(focused.then(|| div().flex_none().ml(px(1.)).w(px(2.)).h(px(14.)).bg(theme::accent_text())));
    field(focused)
        .w_full()
        .relative()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, _, window, cx| {
                window.focus(&handle, cx);
                window.prevent_default();
            }),
        )
        .child(div().absolute().left(px(8.)).top(px(5.)).w(px(64.)).flex().text_color(gpui::transparent_black()).child(input.clone()))
        .child(cover)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_change_is_checked_before_it_is_sent() {
        assert_eq!(password_problem("", "a new password", "a new password"), Some(NO_CURRENT));
        assert_eq!(password_problem("old", "short", "short"), Some(SIZE));
        assert_eq!(password_problem("old", &"x".repeat(129), &"x".repeat(129)), Some(SIZE));
        assert_eq!(password_problem("old", "a new password", "a new passwore"), Some(MISMATCH));
        assert_eq!(password_problem("old", "a new password", "a new password"), None);
        assert_eq!(password_problem("old", &"x".repeat(128), &"x".repeat(128)), None);
    }
}
