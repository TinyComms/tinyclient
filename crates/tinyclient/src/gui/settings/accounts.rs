//! Settings › Accounts (08 §5 and §6, with 01's sizes): one card per network, a row per
//! account with its state in words, Sign in, Sign in again and Sign out… A connected Tiny server
//! account also has its name and Sign out everywhere (`tiny.rs`).

use super::controls::{caption, card, group_label, rule};
use super::{Confirm, SettingsWindow};
use crate::gui::components::{button, link_word, network_icon, ButtonKind};
use crate::gui::layout::NETWORK_ORDER;
use crate::gui::phone::{self, tokens::{self, PhoneType}};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, SharedString, Window};
use protocol::{AccountStatus, Link, Network};

pub fn network_name(network: Network) -> &'static str {
    match network {
        Network::Teams => "Microsoft Teams",
        other => other.label(),
    }
}

/// An account's state in words: the link, or what stands in its way.
fn account_state(account: &AccountStatus) -> String {
    if !account.required.is_empty() {
        return "Needs attention in its own app".to_owned();
    }
    match account.link {
        Link::Offline if !account.reconnecting => "Offline".to_owned(),
        link => link_word(link).to_owned(),
    }
}

fn dot(account: &AccountStatus) -> Div {
    let color = match account.link {
        Link::Connected => theme::positive(),
        Link::Offline => theme::warning(),
        Link::Expired | Link::SignedOut => theme::text_faint(),
    };
    div().size(px(6.)).rounded_full().bg(color).flex_none()
}

fn account_row(network: Network, name: SharedString, line: SharedString, dot: Option<Div>, action: AnyElement) -> Div {
    if phone::active() {
        return div()
            .min_h(px(68.))
            .px(px(tokens::GUTTER))
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .child(network_icon(network, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().phone_row().font_weight(theme::MEDIUM).text_color(theme::text()).truncate().child(name))
                    .child(div().flex().items_center().gap(px(6.)).children(dot).child(div().min_w_0().truncate().phone_preview().text_color(theme::text_soft()).child(line))),
            )
            .child(action);
    }
    div()
        .min_h(px(52.))
        .px(px(12.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .size(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(network_icon(network, 24.)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().text_color(theme::text()).truncate().child(name))
                .child(div().flex().items_center().gap(px(6.)).children(dot).child(div().type_small().text_color(theme::text_soft()).child(line))),
        )
        .child(action)
}

impl SettingsWindow {
    pub(super) fn accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // A thumb-sized button on a phone; the desktop's own size elsewhere.
        let touch = |button: gpui::Stateful<Div>| {
            if phone::active() {
                button.h(px(36.)).px(px(16.)).rounded(px(18.)).text_size(px(tokens::scaled(15.)))
            } else {
                button
            }
        };
        let Some(shell) = self.shell.upgrade() else {
            return div();
        };
        let shell = shell.read(cx);
        let accounts = shell.snapshot.as_ref().map(|s| s.accounts.clone()).unwrap_or_default();
        let (signing_in, failed) = (shell.signing_in, shell.sign_in_failed.clone());
        let mut tiny_line = None;
        let mut pane = div().flex().flex_col().gap(px(20.));
        for network in NETWORK_ORDER {
            let mine: Vec<&AccountStatus> = accounts.iter().filter(|a| a.key.network == network && a.link != Link::SignedOut).collect();
            // A phone cannot sign in to Signal or Messages (`signin::welcome::on_phone`): their
            // groups show only for an account that came from elsewhere.
            if phone::active() && mine.is_empty() && !crate::gui::signin::welcome::on_phone(network) {
                continue;
            }
            let mut group = card();
            if mine.is_empty() {
                let waiting = signing_in == Some(network);
                let line = match (waiting, phone::active()) {
                    (true, true) => "Finish signing in on its page.",
                    (true, false) => "Finish signing in in the other window.",
                    _ => "Not signed in",
                };
                let action = if waiting {
                    div().into_any_element()
                } else {
                    touch(button(("sign-in", network as usize), ButtonKind::Secondary, "Sign in"))
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
                        .into_any_element()
                };
                group = group.child(account_row(network, network_name(network).into(), line.into(), None, action));
            }
            for (ix, account) in mine.iter().enumerate() {
                let state = account_state(account);
                let action = if account.link == Link::Expired {
                    touch(button(("sign-in-again", ix + 10 * network as usize), ButtonKind::Primary, "Sign in again"))
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
                        .into_any_element()
                } else {
                    let account = (*account).clone();
                    touch(button(("sign-out", ix + 10 * network as usize), ButtonKind::Ghost, "Sign out…"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm = Some(Confirm::SignOut(Box::new(account.clone())));
                            cx.notify();
                        }))
                        .into_any_element()
                };
                let name: SharedString = if account.name.is_empty() { network_name(network).into() } else { account.name.clone().into() };
                let line = format!("{} · {}", network_name(network), state);
                group = group.when(ix > 0, |group| group.child(rule())).child(account_row(network, name, line.into(), Some(dot(account)), action));
                if network == Network::Tiny && account.link == Link::Connected {
                    group = group.child(self.tiny_rows(account, ix, window, cx));
                }
            }
            if network == Network::Tiny {
                tiny_line = self.tiny_line();
            }
            pane = pane.child(
                div()
                    .flex()
                    .flex_col()
                    .child(group_label(network_name(network)))
                    .child(group)
                    .children(failed.as_ref().filter(|(at, _)| *at == network).map(|(_, words)| {
                        div().pt(px(6.)).px(px(super::controls::side())).type_small().text_color(theme::danger()).child(words.clone())
                    }))
                    .children(tiny_line.take()),
            );
        }
        pane.child(caption(crate::host::sign_out_caption()).pt(px(0.)))
    }

    fn sign_in(&mut self, network: Network, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.sign_in(network, cx));
        cx.notify();
    }

    pub(super) fn sign_out(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.sign_out(account, cx));
        cx.notify();
    }

    /// The sign-out question (§6), over the whole window.
    pub(super) fn sign_out_modal(&self, account: &AccountStatus, cx: &mut Context<Self>) -> Div {
        let network = network_name(account.key.network);
        let name = if account.name.is_empty() { network.to_owned() } else { account.name.clone() };
        let key = account.key.clone();
        super::modal_panel(
            format!("Sign out of {name}?"),
            vec![
                crate::host::sign_out_body(network),
                format!("Nothing is deleted on {network}. Your other accounts are not touched."),
            ],
            button("sign-out-confirm", ButtonKind::Danger, "Sign out").on_click(cx.listener(move |this, _, _, cx| {
                this.confirm = None;
                this.sign_out(key.clone(), cx);
            })),
            cx,
        )
    }
}
