//! Settings › Accounts, each Tiny server's own rows: the name, and signing out of every device.
//! They show only while that account is connected. The account is named by its host, so the name
//! shown here is read from the server's member list. There is no password here: the server's
//! sign-in provider keeps it.

use super::controls::{rule, row};
use super::{Confirm, SettingsWindow};
use crate::gui::components::{button, button_disabled, field, ButtonKind};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Entity, Focusable, Subscription, Window};
use protocol::{AccountKey, AccountStatus, Link, Network, Request, Response};
use std::collections::HashMap;

const NAME_SIZE: &str = "A name is 1 to 80 characters.";

pub(super) struct TinyAccount {
    name: Entity<TextInput>,
    /// The account whose name field is open.
    naming: Option<AccountKey>,
    /// Each connected server's display name for this account, once the server said. `None`
    /// while it is being asked, empty when it did not say. Asked again when Accounts is opened.
    names: HashMap<AccountKey, Option<String>>,
    busy: bool,
    /// Under the card: a failed rename or sign-out, or what just changed.
    line: Option<(String, bool)>,
}

impl TinyAccount {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> (TinyAccount, Vec<Subscription>) {
        let name = cx.new(|cx| TextInput::new("Name", cx));
        let subscriptions = vec![
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
        let account = TinyAccount { name, naming: None, names: HashMap::new(), busy: false, line: None };
        (account, subscriptions)
    }

    /// Forgets the names read, so opening Accounts asks each server again.
    pub(super) fn forget_names(&mut self) {
        self.names.clear();
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
        let everywhere = Box::new(account.clone());
        div()
            .flex()
            .flex_col()
            .child(rule())
            .child(name_row)
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

    pub(super) fn tiny_everywhere_modal(&self, account: &AccountStatus, cx: &mut Context<Self>) -> Div {
        let key = account.key.clone();
        // The account is named by its host.
        let host = if account.name.is_empty() { "this Tiny server".to_owned() } else { account.name.clone() };
        super::modal_panel(
            "Sign out everywhere?",
            vec![
                format!("Every device signed in to {host} as you is signed out, this one too."),
                "Sign in again in your browser.".to_owned(),
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
