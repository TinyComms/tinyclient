//! Sign-in › Passkeys: this account's own, read each time the Sign-in page comes on show and
//! again after anything done to them. Each can be renamed in place or removed (asked first; the
//! last one stays while no recovery code is left). "Add a passkey" opens the server's page for it
//! in the browser, as setup's "Add a passkey for me" does, and "New recovery codes" shows a fresh
//! set once, in a modal. A server older than all this says so to the read, and then the section
//! is not drawn.

use super::kit::{card, empty, input, now, note};
use super::section;
use crate::client::ask;
use crate::gui::components::{button, button_disabled, tip, ButtonKind};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::channel::chip;
use crate::gui::tiny::model;
use crate::gui::tiny::Modal;
use gpui::{div, prelude::*, px, Context, Div, Entity, Focusable, Window};
use protocol::tiny_words::{LAST_PASSKEY, OLDER};
use protocol::{Request, Response, TinyPasskey, TinyPasskeys};
use std::io;

/// Said once a link to add a passkey has opened in the browser.
const LINK_OPENED: &str = "A page to add a passkey opened in your browser. When you have added one, come back and check again.";

/// What the Passkeys section holds, on the server page's state.
#[derive(Default)]
pub(in crate::gui::tiny) struct Held {
    /// This account's passkeys, as last read.
    pub list: Option<TinyPasskeys>,
    /// What the server said no to, in words: drawn as a danger note.
    pub error: Option<String>,
    /// The server is older than a member's own passkeys: the section is not drawn.
    pub older: bool,
    /// A link, a rename, a removal or new codes is with the server.
    pub busy: bool,
    /// A link to add a passkey opened in the browser: said, with "Check again".
    pub note: Option<String>,
    /// The passkey being renamed, by id, and the field its new name is typed in.
    pub renaming: Option<(String, Entity<TextInput>)>,
    /// Counts reads, so only the newest one's answer is taken.
    read_epoch: u64,
}

/// What the section draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drawn {
    /// Nothing: not read yet, or the server is older than all this.
    Hidden,
    /// The first read failed: what it said, and "Try again".
    Failed,
    /// The list, empty or not.
    List,
}

impl Held {
    fn drawn(&self) -> Drawn {
        match (&self.list, &self.error) {
            _ if self.older => Drawn::Hidden,
            (Some(_), _) => Drawn::List,
            (None, Some(_)) => Drawn::Failed,
            (None, None) => Drawn::Hidden,
        }
    }

    /// The answer to a read: the list, the server's being older (only chatd's own words for it,
    /// `tiny_words::OLDER`), or what went wrong.
    fn take_read(&mut self, answer: io::Result<Response>) {
        match answer {
            Ok(Response::TinyPasskeys { passkeys }) => {
                self.list = Some(passkeys);
                self.error = None;
                self.older = false;
            }
            Ok(Response::Error { detail, .. }) if detail.trim() == OLDER => {
                self.list = None;
                self.error = None;
                self.older = true;
            }
            other => self.error = Some(trouble(other)),
        }
    }
}

/// Words for an answer that was not the one asked for.
fn trouble(answer: io::Result<Response>) -> String {
    match answer {
        Ok(Response::Error { detail, .. }) => detail,
        Ok(_) => crate::client::OTHER_ANSWER.to_owned(),
        Err(err) => crate::client::trouble_words("tiny", &err),
    }
}

/// `Ok` was asked for: `None` when it came, else what came instead in words.
fn refused(answer: io::Result<Response>) -> Option<String> {
    match answer {
        Ok(Response::Ok) => None,
        other => Some(trouble(other)),
    }
}

/// Removing this one is refused before it is asked: it is the only passkey and no recovery code
/// is left to sign in with instead.
fn keeps_last(passkeys: &TinyPasskeys) -> bool {
    passkeys.passkeys.len() <= 1 && passkeys.recovery_codes_left == 0
}

/// The name to send for a rename: what was typed, trimmed; `None` when that is nothing or the
/// name it has now, which is no change.
fn rename_label(typed: &str, current: &str) -> Option<String> {
    let typed = typed.trim();
    (!typed.is_empty() && typed != current.trim()).then(|| typed.to_owned())
}

/// "Made Oct 4, 2026 · Last used 2 days ago", as Members has Joined and Last seen.
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
fn passkey_name(label: &str) -> String {
    match label.trim() {
        "" => "Passkey".to_owned(),
        name => name.to_owned(),
    }
}

/// The empty list's line, naming the provider when the server does.
fn none_yet(provider: &str) -> String {
    match provider.trim() {
        "" => "No passkeys yet. Add one to sign in with it.".to_owned(),
        name => format!("No passkeys yet. Add one to sign in without {name}."),
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

/// The Passkeys section, between Provider and Sessions; `None` until the first read answers, and
/// on a server older than a member's own passkeys.
pub(super) fn passkeys(shell: &Shell, provider: &str, window: &mut Window, cx: &mut Context<Shell>) -> Option<Div> {
    let held = &shell.tiny_state.passkeys;
    let mut section = section("Passkeys");
    let list = match (held.drawn(), held.list.as_ref()) {
        (Drawn::List, Some(list)) => list,
        (Drawn::Failed, _) => {
            let detail = held.error.clone().unwrap_or_default();
            return Some(section.child(note("icons/circle-alert.svg", theme::danger(), detail)).child(
                div().flex().child(button("tiny-signin-passkeys-retry", ButtonKind::Secondary, "Try again").on_click(cx.listener(|this, _, _, cx| this.tiny_passkeys_read(cx)))),
            ));
        }
        _ => return None,
    };
    if list.passkeys.is_empty() {
        section = section.child(empty("icons/key.svg", none_yet(provider)));
    } else {
        let now = now();
        let mut rows = card();
        for (ix, passkey) in list.passkeys.iter().enumerate() {
            rows = rows.child(passkey_row(held, list, passkey, ix, now, window, cx));
        }
        section = section.child(rows);
    }
    if let Some(detail) = &held.error {
        section = section.child(note("icons/circle-alert.svg", theme::danger(), detail.clone()));
    }
    let (add, codes) = if held.busy {
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
    if let Some(words) = &held.note {
        section = section.child(
            div().flex().flex_col().gap(px(8.)).child(note("icons/info.svg", theme::text_faint(), words.clone())).child(
                div().flex().child(button("tiny-signin-passkeys-check", ButtonKind::Secondary, "Check again").on_click(cx.listener(|this, _, _, cx| {
                    this.tiny_state.passkeys.note = None;
                    this.tiny_passkeys_read(cx);
                }))),
            ),
        );
    }
    Some(section)
}

/// One passkey: its name (or the field to rename it in), when it was made and last used, whether
/// it is synced or flagged, and Rename and Remove.
fn passkey_row(held: &Held, list: &TinyPasskeys, passkey: &TinyPasskey, ix: usize, now: i64, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    let editing = held.renaming.as_ref().filter(|(id, _)| *id == passkey.id).map(|(_, field)| field.clone());
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
        let save = if held.busy {
            button_disabled("Save").into_any_element()
        } else {
            button("tiny-signin-passkey-save", ButtonKind::Primary, "Save").on_click(cx.listener(|this, _, _, cx| this.tiny_passkey_rename_save(cx))).into_any_element()
        };
        actions = actions.child(button("tiny-signin-passkey-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| this.tiny_passkey_rename_cancel(cx)))).child(save);
    } else if held.busy {
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

impl Shell {
    /// The Sign-in page comes on show: what was being typed or said goes, and the passkeys are read.
    pub(in crate::gui::tiny) fn tiny_signin_enter(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.passkeys.renaming = None;
        self.tiny_state.passkeys.note = None;
        self.tiny_passkeys_read(cx);
    }

    /// The account an answer was asked for is still the one on show.
    fn tiny_still(&self, account: &protocol::AccountKey) -> bool {
        self.tiny_state.account.as_ref() == Some(account)
    }

    /// This account's own passkeys. Only the newest read's answer is taken, and only while its
    /// account is on show.
    pub(in crate::gui::tiny) fn tiny_passkeys_read(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let held = &mut self.tiny_state.passkeys;
        held.read_epoch += 1;
        let epoch = held.read_epoch;
        let asked = account.clone();
        self.off_thread(cx, move || ask(&Request::TinyPasskeysRead { account: asked }), move |this, answer, cx| {
            if this.tiny_state.passkeys.read_epoch != epoch || !this.tiny_still(&account) {
                return;
            }
            this.tiny_state.passkeys.take_read(answer);
            cx.notify();
        });
    }

    /// Asks the server for a link that adds a passkey for this account. chatd opens it in the
    /// system browser; what came of it is said where it was asked from. From Sign-in a link that
    /// opened reads the passkeys again; a refusal is said as one.
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
            LinkFrom::SignIn if state.passkeys.busy => return,
            LinkFrom::SignIn => {
                state.passkeys.busy = true;
                state.passkeys.note = None;
                state.passkeys.error = None;
            }
        }
        cx.notify();
        let asked = account.clone();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyLink { account: asked }), move |this, answer, cx| {
            if !this.tiny_still(&account) {
                return;
            }
            let failed = refused(answer);
            match from {
                LinkFrom::Setup => {
                    let setup = &mut this.tiny_state.setup;
                    setup.linking = false;
                    setup.link_note = Some(failed.unwrap_or_else(|| LINK_OPENED.to_owned()));
                }
                LinkFrom::SignIn => {
                    this.tiny_state.passkeys.busy = false;
                    match failed {
                        None => {
                            this.tiny_state.passkeys.note = Some(LINK_OPENED.to_owned());
                            this.tiny_passkeys_read(cx);
                        }
                        Some(detail) => this.tiny_state.passkeys.error = Some(detail),
                    }
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
        self.tiny_state.passkeys.error = None;
        self.tiny_state.passkeys.renaming = Some((id, field));
        cx.notify();
    }

    fn tiny_passkey_rename_cancel(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.passkeys.renaming = None;
        self.tiny_state.passkeys.error = None;
        cx.notify();
    }

    /// The new name to the server ([`rename_label`]); no change only closes the field.
    fn tiny_passkey_rename_save(&mut self, cx: &mut Context<Self>) {
        let (Some(account), Some((id, field))) = (self.tiny_state.account.clone(), self.tiny_state.passkeys.renaming.clone()) else {
            return;
        };
        if self.tiny_state.passkeys.busy {
            return;
        }
        let current = self.tiny_state.passkeys.list.as_ref().and_then(|list| list.passkeys.iter().find(|passkey| passkey.id == id)).map(|passkey| passkey.label.clone()).unwrap_or_default();
        let Some(label) = rename_label(field.read(cx).text(), &current) else {
            self.tiny_passkey_rename_cancel(cx);
            return;
        };
        self.tiny_state.passkeys.busy = true;
        self.tiny_state.passkeys.error = None;
        cx.notify();
        let asked = account.clone();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyRename { account: asked, id, label }), move |this, answer, cx| {
            if !this.tiny_still(&account) {
                return;
            }
            this.tiny_state.passkeys.busy = false;
            // A refusal stays said: a read now would clear it.
            match refused(answer) {
                None => {
                    this.tiny_state.passkeys.renaming = None;
                    this.tiny_passkeys_read(cx);
                }
                Some(detail) => this.tiny_state.passkeys.error = Some(detail),
            }
            cx.notify();
        });
    }

    /// From the modal: the passkey goes. A refusal is said in the modal, which stays; either way
    /// the list is read again (a passkey already gone is gone from it too).
    pub(in crate::gui::tiny) fn tiny_passkey_remove(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if self.tiny_state.passkeys.busy {
            return;
        }
        self.tiny_state.passkeys.busy = true;
        self.tiny_state.modal_error = None;
        cx.notify();
        let asked = account.clone();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyRemove { account: asked, id }), move |this, answer, cx| {
            if !this.tiny_still(&account) {
                return;
            }
            this.tiny_state.passkeys.busy = false;
            match refused(answer) {
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
        if self.tiny_state.passkeys.busy {
            return;
        }
        let held = &mut self.tiny_state.passkeys;
        held.busy = true;
        held.error = None;
        held.note = None;
        cx.notify();
        let asked = account.clone();
        self.off_thread(cx, move || ask(&Request::TinyRecoveryCodes { account: asked }), move |this, answer, cx| {
            if !this.tiny_still(&account) {
                return;
            }
            this.tiny_state.passkeys.busy = false;
            match answer {
                Ok(Response::TinyRecoveryCodes { codes }) => {
                    this.tiny_show_codes(codes, cx);
                    this.tiny_passkeys_read(cx);
                }
                other => this.tiny_state.passkeys.error = Some(trouble(other)),
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
    use protocol::ErrorCode;

    fn passkey(id: &str) -> TinyPasskey {
        TinyPasskey { id: id.into(), label: String::new(), created_at: "2026-10-01T09:00:00Z".into(), last_used_at: None, synced: false, flagged: false }
    }

    fn said(detail: &str) -> io::Result<Response> {
        Ok(Response::Error { code: ErrorCode::Refused, detail: detail.to_owned() })
    }

    #[test]
    fn an_older_server_hides_the_section_and_any_other_refusal_is_said() {
        let mut held = Held::default();
        assert_eq!(held.drawn(), Drawn::Hidden, "nothing before the first answer");
        held.take_read(said(protocol::tiny_words::OLDER));
        assert_eq!(held.drawn(), Drawn::Hidden);
        assert!(held.older && held.error.is_none());

        let mut held = Held::default();
        held.take_read(said("Couldn't reach the server."));
        assert_eq!(held.drawn(), Drawn::Failed);
        assert_eq!(held.error.as_deref(), Some("Couldn't reach the server."));

        held.take_read(Ok(Response::TinyPasskeys { passkeys: TinyPasskeys { passkeys: vec![passkey("a")], recovery_codes_left: 3 } }));
        assert_eq!(held.drawn(), Drawn::List);
        assert!(held.error.is_none(), "a read that answers clears the last read's trouble");
        held.take_read(Ok(Response::Ok));
        assert_eq!(held.drawn(), Drawn::List, "a later failure keeps the list and says why");
        assert_eq!(held.error.as_deref(), Some(crate::client::OTHER_ANSWER));
    }

    #[test]
    fn only_ok_is_a_link_or_a_change_done() {
        assert_eq!(refused(Ok(Response::Ok)), None);
        assert_eq!(refused(said(LAST_PASSKEY)).as_deref(), Some(LAST_PASSKEY));
        let codes = Ok(Response::TinyRecoveryCodes { codes: vec!["a".into()] });
        assert_eq!(refused(codes).as_deref(), Some(crate::client::OTHER_ANSWER), "codes are not what a link answers");
    }

    #[test]
    fn a_rename_sends_what_was_typed_trimmed_and_nothing_for_no_change() {
        assert_eq!(rename_label("  Work laptop ", "1Password"), Some("Work laptop".to_owned()));
        assert_eq!(rename_label("   ", "1Password"), None, "nothing typed");
        assert_eq!(rename_label(" 1Password", "1Password "), None, "the name it has");
        assert_eq!(rename_label("Key", ""), Some("Key".to_owned()), "a passkey with no name gets one");
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
        assert_eq!(none_yet("Pocket ID"), "No passkeys yet. Add one to sign in without Pocket ID.");
        assert_eq!(none_yet(""), "No passkeys yet. Add one to sign in with it.");
    }
}
