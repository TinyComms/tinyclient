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
                    let can_edit = account.link == Link::Connected && account.can.can_edit_profile();
                    let edit_btn = can_edit.then(|| {
                        let acc = account.clone();
                        touch(button(("edit-profile", ix + 10 * network as usize), ButtonKind::Secondary, "Edit profile…"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm = Some(Confirm::EditProfile(Box::new(acc.clone())));
                                cx.notify();
                            }))
                    });
                    let sign_out_btn = touch(button(("sign-out", ix + 10 * network as usize), ButtonKind::Ghost, "Sign out…"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm = Some(Confirm::SignOut(Box::new(account.clone())));
                            cx.notify();
                        }));
                    div().flex().items_center().gap(px(6.)).children(edit_btn).child(sign_out_btn).into_any_element()
                };
                let name: SharedString = if account.name.is_empty() { network_name(network).into() } else { account.name.clone().into() };
                let line = format!("{} · {}", network_name(network), state);
                group = group.when(ix > 0, |group| group.child(rule())).child(account_row(network, name, line.into(), Some(dot(account)), action));
                if network == Network::Tiny && account.link == Link::Connected {
                    group = group.child(self.tiny_rows(account, ix, window, cx));
                }
                if network == Network::Telegram && account.link == Link::Connected {
                    group = group.child(self.telegram_call_privacy_rows(account, ix, cx));
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

    fn load_telegram_privacy(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.tg_privacy_requested.insert(account.clone()) {
            return;
        }
        let keys = [
            protocol::PrivacyKey::PhoneCall,
            protocol::PrivacyKey::PhoneP2p,
            protocol::PrivacyKey::VoiceMessages,
            protocol::PrivacyKey::LastSeen,
            protocol::PrivacyKey::PhoneNumber,
            protocol::PrivacyKey::ProfilePhoto,
            protocol::PrivacyKey::ChatInvite,
        ];
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::new();
                    for key in keys {
                        if let Ok(protocol::Response::Privacy { policy, .. }) =
                            crate::client::ask(&protocol::Request::GetPrivacy { account: acc.clone(), key })
                        {
                            out.push((key, policy));
                        }
                    }
                    out
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for (key, policy) in loaded {
                    this.tg_privacy.insert((account.clone(), key), policy);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_telegram_privacy_audience(
        &mut self,
        account: protocol::AccountKey,
        key: protocol::PrivacyKey,
        audience: protocol::PrivacyAudience,
        cx: &mut Context<Self>,
    ) {
        // Never write a rule list that was not fully read first.
        let Some(mut policy) = self.tg_privacy.get(&(account.clone(), key)).cloned() else {
            return;
        };
        if policy.premium_only || policy.audience == audience {
            return;
        }
        policy.audience = audience;
        self.tg_privacy.insert((account.clone(), key), policy.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let sent = policy.clone();
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&protocol::Request::SetPrivacy { account: acc, key, policy: sent })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(protocol::Response::Privacy { policy, .. }) = answer {
                    this.tg_privacy.insert((account, key), policy);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn telegram_call_privacy_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_telegram_privacy(account.key.clone(), cx);
        let scope = protocol::settings::account_scope(&account.key);
        let always_relay = crate::gui::app::prefs::values()
            .stored(&scope, protocol::settings::TELEGRAM_ALWAYS_RELAY)
            == Some("on");
        let mut box_div = div().flex().flex_col();
        let rows: [(&str, &str, protocol::PrivacyKey); 7] = [
            ("Who can call me", "Controls who can place voice or video calls to you on Telegram.", protocol::PrivacyKey::PhoneCall),
            ("Peer-to-Peer", "Controls when Telegram may share your IP address directly during a call.", protocol::PrivacyKey::PhoneP2p),
            ("Voice & video messages", "Controls who may send you voice and round video messages.", protocol::PrivacyKey::VoiceMessages),
            ("Who can see my last seen", "Controls who can see when you were last online.", protocol::PrivacyKey::LastSeen),
            ("Who can see my phone number", "Controls who can see the phone number on this account.", protocol::PrivacyKey::PhoneNumber),
            ("Who can see my photo", "Controls who can see your profile photo.", protocol::PrivacyKey::ProfilePhoto),
            ("Who can add me", "Controls who can add you to groups and channels.", protocol::PrivacyKey::ChatInvite),
        ];
        for (row_ix, (title, sub, pkey)) in rows.into_iter().enumerate() {
            let policy = self.tg_privacy.get(&(account.key.clone(), pkey)).cloned();
            let premium_locked = policy.as_ref().is_some_and(|p| p.premium_only);
            let exceptions_note = policy.as_ref().and_then(|p| {
                let mut parts = Vec::new();
                if !p.always_allow.is_empty() {
                    parts.push(format!("Always allow: {}", p.always_allow.len()));
                }
                if !p.never_allow.is_empty() {
                    parts.push(format!("Never allow: {}", p.never_allow.len()));
                }
                (!parts.is_empty()).then(|| parts.join(" · "))
            });
            let choices: [(&str, protocol::PrivacyAudience); 3] = [
                ("Everybody", protocol::PrivacyAudience::Everybody),
                ("Contacts", protocol::PrivacyAudience::Contacts),
                ("Nobody", protocol::PrivacyAudience::Nobody),
            ];
            let control = if let Some(ref pol) = policy {
                if premium_locked {
                    div().type_small().text_color(theme::text_faint()).child("Telegram Premium").into_any_element()
                } else {
                    let mut seg = div().flex().items_center().gap(px(4.));
                    for (c_ix, (label, aud)) in choices.into_iter().enumerate() {
                        let active = pol.audience == aud;
                        let acc_key = account.key.clone();
                        seg = seg.child(
                            button(
                                ("tg-priv", ix * 100 + row_ix * 10 + c_ix),
                                if active { ButtonKind::Primary } else { ButtonKind::Secondary },
                                label,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_telegram_privacy_audience(acc_key.clone(), pkey, aud, cx);
                            })),
                        );
                    }
                    seg.into_any_element()
                }
            } else {
                div().type_small().text_color(theme::text_faint()).child("Loading…").into_any_element()
            };
            box_div = box_div.child(rule()).child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().type_ui().text_color(theme::text()).child(title))
                            .child(div().type_small().text_color(theme::text_soft()).child(sub))
                            .children(exceptions_note.map(|note| div().type_small().text_color(theme::text_faint()).child(note))),
                    )
                    .child(control),
            );
        }
        // Local "Always relay my Telegram calls" switch (independent of Telegram account P2P setting).
        let relay_scope = scope.clone();
        box_div = box_div.child(rule()).child(
            div()
                .px(px(12.))
                .py(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().type_ui().text_color(theme::text()).child("Always relay my Telegram calls"))
                        .child(div().type_small().text_color(theme::text_soft()).child("Never offer a direct peer-to-peer path on this device, regardless of your Telegram setting.")),
                )
                .child(
                    super::controls::switch(("tg-always-relay", ix), "Always relay my Telegram calls", always_relay).on_click(cx.listener(move |this, _, _, cx| {
                        let next = !always_relay;
                        let stored = next.then(|| "on".to_owned());
                        this.set("tg-always-relay", relay_scope.clone(), protocol::settings::TELEGRAM_ALWAYS_RELAY, stored, cx);
                    })),
                ),
        );
        box_div
    }

    pub(super) fn sign_out(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.sign_out(account, false, cx));
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

    /// Own-profile editor modal: shows only fields supported by `account.can`.
    pub(super) fn edit_profile_modal(&self, account: &AccountStatus, cx: &mut Context<Self>) -> Div {
        let network = network_name(account.key.network);
        let key = account.key.clone();
        let can = account.can;
        let field_box = |label: &'static str, input: gpui::Entity<crate::gui::input::TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().type_small().text_color(theme::text_soft()).child(label))
                .child(
                    div()
                        .h(px(30.))
                        .px(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::field())
                        .border_1()
                        .border_color(theme::border())
                        .flex()
                        .items_center()
                        .child(input),
                )
        };
        let mut fields = div().flex().flex_col().gap(px(10.));
        if can.profile_name {
            fields = fields.child(field_box("Display name", self.profile_name.clone()));
        }
        if can.profile_about {
            fields = fields.child(field_box("About", self.profile_about.clone()));
        }
        if can.profile_pronouns {
            fields = fields.child(field_box("Pronouns", self.profile_pronouns.clone()));
        }
        if can.profile_avatar {
            let avatar_key = key.clone();
            fields = fields.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_small().text_color(theme::text_soft()).child("Profile picture"))
                    .child(button("profile-pick-avatar", ButtonKind::Secondary, "Choose picture…").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let avatar_key = avatar_key.clone();
                            let asked = cx.prompt_for_paths(gpui::PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: Some("Choose profile picture".into()),
                            });
                            cx.spawn(async move |this, cx| {
                                if let Ok(Ok(Some(paths))) = asked.await {
                                    if let Some(path) = paths.into_iter().next() {
                                        let path_str = path.to_string_lossy().into_owned();
                                        let req = protocol::Request::SetAvatar { account: avatar_key, path: path_str };
                                        let _ = cx.background_executor().spawn(async move { crate::client::ask(&req) }).await;
                                        let _ = this.update(cx, |_, cx| cx.notify());
                                    }
                                }
                            })
                            .detach();
                        },
                    ))),
            );
        }
        let save_key = key.clone();
        let save = button("profile-save", ButtonKind::Primary, "Save").on_click(cx.listener(move |this, _, _, cx| {
            let name = can.profile_name.then(|| this.profile_name.read(cx).text().trim().to_owned()).filter(|s| !s.is_empty());
            let about = can.profile_about.then(|| this.profile_about.read(cx).text().trim().to_owned());
            let pronouns = can.profile_pronouns.then(|| this.profile_pronouns.read(cx).text().trim().to_owned());
            let req = protocol::Request::SetProfile {
                account: save_key.clone(),
                name,
                about,
                pronouns,
                emoji: None,
                username: None,
                photo: false,
            };
            this.confirm = None;
            this.off_thread(cx, move || crate::client::ask(&req), |_, _, cx| cx.notify());
        }));
        let cancel = button("profile-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| {
            this.confirm = None;
            cx.notify();
        }));
        crate::gui::components::modal(format!("Edit {network} profile"))
            .child(fields)
            .child(crate::gui::components::modal_buttons().child(cancel).child(save))
    }
}
