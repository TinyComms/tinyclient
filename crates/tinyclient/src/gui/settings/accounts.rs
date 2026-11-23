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
            // A phone offers every network supported on phone (`signin::welcome::on_phone`).
            if phone::active() && mine.is_empty() && !crate::gui::signin::welcome::on_phone(network) {
                continue;
            }
            let mut group = card();
            if mine.is_empty() {
                let waiting = signing_in == Some(network);
                let line = match (waiting, phone::active(), network) {
                    (true, true, _) => "Finish signing in on its page.",
                    (true, false, _) => "Finish signing in in the other window.",
                    (false, true, Network::Messages) => "Pair by QR from an Android phone; arrives while open",
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
                                this.profile_space = None;
                                this.profile_accent.update(cx, |input, cx| input.set_text("", cx));
                                this.profile_server_about.update(cx, |input, cx| input.set_text("", cx));
                                this.profile_server_pronouns.update(cx, |input, cx| input.set_text("", cx));
                                this.profile_server_accent.update(cx, |input, cx| input.set_text("", cx));
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
                    group = group.child(self.telegram_download_rows(account, ix, cx));
                }
                if network == Network::Discord {
                    group = group.child(self.discord_account_rows(account, ix, cx));
                    if account.link == Link::Connected {
                        group = group.child(self.discord_privacy_rows(account, ix, cx));
                    }
                }
                if account.can.notify_defaults && account.link == Link::Connected {
                    group = group.child(self.notify_defaults_rows(account, ix, cx));
                }
                if network == Network::Telegram && account.can.active_sessions && account.link == Link::Connected {
                    group = group.child(self.active_sessions_rows(account, ix, cx));
                }
            }
            if network == Network::Discord && !mine.is_empty() {
                let action = touch(button(("add-discord", network as usize), ButtonKind::Secondary, "Add another"))
                    .on_click(cx.listener(|this, _, _, cx| this.sign_in(Network::Discord, cx)))
                    .into_any_element();
                group = group.child(rule()).child(account_row(
                    network,
                    "Add another Discord account".into(),
                    "Sign in to another account".into(),
                    None,
                    action,
                ));
            }
            if network == Network::Teams {
                let action = touch(button(("teams-personal", network as usize), ButtonKind::Secondary, "Sign in"))
                    .on_click(cx.listener(|this, _, _, cx| this.sign_in_personal(cx)))
                    .into_any_element();
                group = group.child(rule()).child(account_row(
                    network,
                    "Personal Microsoft account".into(),
                    "Teams free, teams.live.com".into(),
                    None,
                    action,
                ));
            }
            if network == Network::Teams && !mine.is_empty() {
                group = group.child(self.teams_sessions_row(cx));
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

    fn sign_in_personal(&mut self, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.sign_in_personal(cx));
        cx.notify();
    }

    fn load_active_sessions(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.tg_sessions_requested.insert(account.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let answer = cx
                .background_executor()
                .spawn(async move { crate::client::ask(&protocol::Request::ActiveSessions { account: acc }) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(protocol::Response::ActiveSessions { sessions }) = answer {
                    this.tg_sessions.insert(account, sessions);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn end_active_session(&mut self, account: protocol::AccountKey, session_id: String, cx: &mut Context<Self>) {
        self.tg_session_confirm = None;
        self.tg_session_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let sid = session_id.clone();
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&protocol::Request::EndSession { account: acc, session_id: sid })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(protocol::Response::Ok) => {
                        if let Some(list) = this.tg_sessions.get_mut(&account) {
                            list.retain(|s| s.id != session_id);
                        }
                    }
                    Ok(protocol::Response::Error { detail, .. }) => {
                        this.tg_session_error = Some(detail);
                    }
                    Err(err) => {
                        this.tg_session_error = Some(crate::client::trouble_words("end session", &err));
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn active_sessions_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_active_sessions(account.key.clone(), cx);
        let sessions = self.tg_sessions.get(&account.key).cloned();
        let mut box_div = div().flex().flex_col();
        box_div = box_div.child(rule()).child(
            div()
                .px(px(12.))
                .py(px(6.))
                .type_small()
                .font_weight(theme::MEDIUM)
                .text_color(theme::text_soft())
                .child("Devices"),
        );
        if let Some(ref err) = self.tg_session_error {
            box_div = box_div.child(
                div().px(px(12.)).pb(px(6.)).type_small().text_color(theme::danger()).child(err.clone()),
            );
        }
        match sessions {
            None => {
                box_div = box_div.child(
                    div().px(px(12.)).pb(px(8.)).type_small().text_color(theme::text_faint()).child("Loading devices…"),
                );
            }
            Some(list) if list.is_empty() => {
                box_div = box_div.child(
                    div().px(px(12.)).pb(px(8.)).type_small().text_color(theme::text_faint()).child("No active sessions."),
                );
            }
            Some(list) => {
                for (s_ix, sess) in list.iter().enumerate() {
                    let title = if sess.device.is_empty() {
                        sess.app.clone()
                    } else if sess.app.is_empty() {
                        sess.device.clone()
                    } else {
                        format!("{} · {}", sess.device, sess.app)
                    };
                    let detail = if sess.location.is_empty() {
                        sess.platform.clone()
                    } else if sess.platform.is_empty() {
                        sess.location.clone()
                    } else {
                        format!("{} · {}", sess.location, sess.platform)
                    };
                    let confirming = self.tg_session_confirm.as_ref() == Some(&(account.key.clone(), sess.id.clone()));
                    let control = if sess.current {
                        div().type_small().text_color(theme::accent_text()).child("This device").into_any_element()
                    } else if confirming {
                        let acc_ok = account.key.clone();
                        let sid_ok = sess.id.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                button(("tg-sess-go", ix * 100 + s_ix), ButtonKind::Primary, "End session").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.end_active_session(acc_ok.clone(), sid_ok.clone(), cx);
                                    }),
                                ),
                            )
                            .child(
                                button(("tg-sess-no", ix * 100 + s_ix), ButtonKind::Ghost, "Cancel").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.tg_session_confirm = None;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .into_any_element()
                    } else {
                        let acc_ask = account.key.clone();
                        let sid_ask = sess.id.clone();
                        button(("tg-sess-end", ix * 100 + s_ix), ButtonKind::Ghost, "End").on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.tg_session_confirm = Some((acc_ask.clone(), sid_ask.clone()));
                                cx.notify();
                            },
                        ))
                        .into_any_element()
                    };
                    box_div = box_div.child(
                        div()
                            .px(px(12.))
                            .py(px(6.))
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
                                    .child(div().type_ui().text_color(theme::text()).truncate().child(title))
                                    .when(!detail.is_empty(), |d| {
                                        d.child(div().type_small().text_color(theme::text_soft()).truncate().child(detail))
                                    }),
                            )
                            .child(control),
                    );
                }
            }
        }
        box_div
    }

    fn load_notify_defaults(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.tg_notify_requested.insert(account.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let answer = cx
                .background_executor()
                .spawn(async move { crate::client::ask(&protocol::Request::GetNotifyDefaults { account: acc }) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(protocol::Response::NotifyDefaults { defaults }) = answer {
                    this.tg_notify_defaults.insert(account, defaults);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_notify_default(
        &mut self,
        account: protocol::AccountKey,
        scope: protocol::NotifyDefaultScope,
        muted: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(cur) = self.tg_notify_defaults.get_mut(&account) {
            match scope {
                protocol::NotifyDefaultScope::Users => cur.users_muted = muted,
                protocol::NotifyDefaultScope::Groups => cur.groups_muted = muted,
                protocol::NotifyDefaultScope::Channels => cur.channels_muted = muted,
            }
            cx.notify();
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&protocol::Request::SetNotifyDefault { account: acc, scope, muted })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(protocol::Response::NotifyDefaults { defaults }) = answer {
                    this.tg_notify_defaults.insert(account, defaults);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reset_notify_defaults(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let answer = cx
                .background_executor()
                .spawn(async move { crate::client::ask(&protocol::Request::ResetNotifyDefaults { account: acc }) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(protocol::Response::NotifyDefaults { defaults }) = answer {
                    this.tg_notify_defaults.insert(account, defaults);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn notify_defaults_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_notify_defaults(account.key.clone(), cx);
        let loaded = self.tg_notify_defaults.get(&account.key).cloned();
        let mut box_div = div().flex().flex_col();
        let scopes: [(&str, protocol::NotifyDefaultScope); 3] = [
            ("Private chats", protocol::NotifyDefaultScope::Users),
            ("Groups", protocol::NotifyDefaultScope::Groups),
            ("Channels", protocol::NotifyDefaultScope::Channels),
        ];
        for (s_ix, (label, scope)) in scopes.into_iter().enumerate() {
            let control = if let Some(ref defs) = loaded {
                let muted = match scope {
                    protocol::NotifyDefaultScope::Users => defs.users_muted,
                    protocol::NotifyDefaultScope::Groups => defs.groups_muted,
                    protocol::NotifyDefaultScope::Channels => defs.channels_muted,
                };
                let acc_on = account.key.clone();
                let acc_off = account.key.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        button(
                            ("tg-ndef-on", ix * 100 + s_ix * 2),
                            if !muted { ButtonKind::Primary } else { ButtonKind::Secondary },
                            "On",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_notify_default(acc_on.clone(), scope, false, cx);
                        })),
                    )
                    .child(
                        button(
                            ("tg-ndef-off", ix * 100 + s_ix * 2 + 1),
                            if muted { ButtonKind::Primary } else { ButtonKind::Secondary },
                            "Muted",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_notify_default(acc_off.clone(), scope, true, cx);
                        })),
                    )
                    .into_any_element()
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
                            .child(div().type_ui().text_color(theme::text()).child(format!("{label} notifications")))
                            .child(
                                div()
                                    .type_small()
                                    .text_color(theme::text_soft())
                                    .child("Account-wide default across all your devices."),
                            ),
                    )
                    .child(control),
            );
        }
        if let Some(ref defs) = loaded {
            let exc_summary = if defs.exceptions.is_empty() {
                "No per-chat notification exceptions.".to_owned()
            } else {
                let names: Vec<String> = defs
                    .exceptions
                    .iter()
                    .take(5)
                    .map(|e| format!("{} ({})", e.title, if e.muted { "muted" } else { "unmuted" }))
                    .collect();
                format!("Exceptions ({}): {}", defs.exceptions.len(), names.join(", "))
            };
            let acc_reset = account.key.clone();
            box_div = box_div.child(rule()).child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().flex_1().min_w_0().type_small().text_color(theme::text_soft()).child(exc_summary))
                    .child(
                        button(("tg-ndef-reset", ix), ButtonKind::Ghost, "Reset all").on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.reset_notify_defaults(acc_reset.clone(), cx);
                            },
                        )),
                    ),
            );
        }
        box_div
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

    fn load_telegram_default_ttl(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.tg_default_ttl_requested.insert(account.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    match crate::client::ask(&protocol::Request::GetDefaultDisappearing { account: acc }) {
                        Ok(protocol::Response::DefaultDisappearing { seconds }) => Some(seconds),
                        _ => None,
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(seconds) = loaded {
                    this.tg_default_ttl.insert(account, seconds);
                    cx.notify();
                } else {
                    this.tg_default_ttl_requested.remove(&account);
                }
            });
        })
        .detach();
    }

    fn set_telegram_default_ttl(&mut self, account: protocol::AccountKey, seconds: u32, cx: &mut Context<Self>) {
        if self.tg_default_ttl.get(&account).copied() == Some(seconds) {
            return;
        }
        self.tg_default_ttl.insert(account.clone(), seconds);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&protocol::Request::SetDefaultDisappearing { account: acc, seconds })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if answer.is_err() || matches!(answer, Ok(protocol::Response::Error { .. })) {
                    this.tg_default_ttl_requested.remove(&account);
                    this.load_telegram_default_ttl(account, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn telegram_call_privacy_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_telegram_privacy(account.key.clone(), cx);
        self.load_telegram_default_ttl(account.key.clone(), cx);
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
        // Account-wide default auto-delete timer for new Telegram chats.
        let default_ttl = self.tg_default_ttl.get(&account.key).copied();
        let ttl_control = if let Some(active) = default_ttl {
            let mut seg = div().flex().flex_wrap().items_center().gap(px(4.));
            for (c_ix, &secs) in protocol::disappearing_presets(Network::Telegram).iter().enumerate() {
                let acc_key = account.key.clone();
                seg = seg.child(
                    button(
                        ("tg-default-ttl", ix * 100 + c_ix),
                        if active == secs { ButtonKind::Primary } else { ButtonKind::Secondary },
                        protocol::format_disappearing(secs),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_telegram_default_ttl(acc_key.clone(), secs, cx);
                    })),
                );
            }
            seg.into_any_element()
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
                        .child(div().type_ui().text_color(theme::text()).child("Default auto-delete timer"))
                        .child(div().type_small().text_color(theme::text_soft()).child("Automatically delete messages in new Telegram chats after this period.")),
                )
                .child(ttl_control),
        );
        box_div = box_div.child(self.telegram_folders_rows(account, ix, cx));
        box_div
    }

    fn teams_sessions_row(&self, _cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .child(rule())
            .child(
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
                            .child(div().type_ui().text_color(theme::text()).child("Microsoft sessions"))
                            .child(div().type_small().text_color(theme::text_soft()).child(
                                "Signing out removes this device's push registration. End Microsoft sessions across devices at myaccount.microsoft.com.",
                            )),
                    )
                    .child(
                        button("teams-manage-sessions", ButtonKind::Secondary, "Manage sessions ↗")
                            .on_click(|_, _, cx| crate::gui::conversation::links::open_external_link("https://myaccount.microsoft.com/", cx)),
                    ),
            )
    }

    fn load_discord_privacy(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.discord_privacy_requested.insert(account.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    match crate::client::ask(&protocol::Request::GetNetworkPrivacy { account: acc }) {
                        Ok(protocol::Response::NetworkPrivacy { privacy }) => Some(privacy),
                        _ => None,
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(privacy) = loaded {
                    this.discord_privacy.insert(account, privacy);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn update_discord_privacy(
        &mut self,
        account: protocol::AccountKey,
        mutate: impl FnOnce(&mut protocol::NetworkPrivacy),
        cx: &mut Context<Self>,
    ) {
        let Some(mut next) = self.discord_privacy.get(&account).cloned() else {
            return;
        };
        let prev = next.clone();
        mutate(&mut next);
        self.discord_privacy.insert(account.clone(), next.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let sent = next.clone();
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&protocol::Request::SetNetworkPrivacy { account: acc, privacy: sent })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(protocol::Response::NetworkPrivacy { privacy }) => {
                        this.discord_privacy.insert(account, privacy);
                    }
                    _ => {
                        this.discord_privacy.insert(account, prev);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn discord_privacy_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_discord_privacy(account.key.clone(), cx);
        let privacy = self.discord_privacy.get(&account.key).cloned();
        let mut box_div = div().flex().flex_col();
        let Some(priv_state) = privacy else {
            return box_div.child(rule()).child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .type_small()
                    .text_color(theme::text_faint())
                    .child("Loading Privacy & Safety…"),
            );
        };
        let row_wrap = |title: &'static str, sub: &'static str, control: AnyElement| {
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
                        .child(div().type_small().text_color(theme::text_soft()).child(sub)),
                )
                .child(control)
        };
        let switches: [(&str, &str, &str, bool, usize); 5] = [
            ("dc-fr-all", "Friend requests from everyone", "Anyone on Discord can send you a friend request.", priv_state.friend_requests_everyone, 0),
            ("dc-fr-fof", "Friend requests from friends of friends", "People who share a mutual friend with you.", priv_state.friend_requests_mutual_friends, 1),
            ("dc-fr-srv", "Friend requests from server members", "Members of servers you share.", priv_state.friend_requests_mutual_spaces, 2),
            ("dc-dm-srv", "Direct messages from server members", "Default for new servers; override individual servers in their Details.", priv_state.allow_dms_from_spaces, 3),
            ("dc-req-srv", "Message requests from server members", "Filter DMs from non-friends into Message Requests by default.", priv_state.allow_message_requests, 4),
        ];
        for (id, title, sub, on, which) in switches {
            let acc = account.key.clone();
            box_div = box_div.child(rule()).child(row_wrap(
                title,
                sub,
                super::controls::switch((id, ix), title, on)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_discord_privacy(
                            acc.clone(),
                            move |p| match which {
                                0 => p.friend_requests_everyone = !on,
                                1 => p.friend_requests_mutual_friends = !on,
                                2 => p.friend_requests_mutual_spaces = !on,
                                3 => p.allow_dms_from_spaces = !on,
                                _ => p.allow_message_requests = !on,
                            },
                            cx,
                        );
                    }))
                    .into_any_element(),
            ));
        }
        let filter_levels: [(&str, u8); 3] = [("Off", 0), ("Non-friends", 1), ("Everyone", 2)];
        let filters: [(&str, &str, u8, bool); 2] = [
            ("Explicit content filter", "Scan and blur explicit media in DMs.", priv_state.explicit_content_filter, true),
            ("DM spam filter", "Filter direct messages from suspected spammers.", priv_state.dm_spam_filter, false),
        ];
        for (f_ix, (title, sub, current, is_explicit)) in filters.into_iter().enumerate() {
            let mut seg = div().flex().items_center().gap(px(4.));
            for (c_ix, (label, level)) in filter_levels.into_iter().enumerate() {
                let acc = account.key.clone();
                let active = current == level;
                seg = seg.child(
                    button(
                        ("dc-filter", ix * 100 + f_ix * 10 + c_ix),
                        if active { ButtonKind::Primary } else { ButtonKind::Secondary },
                        label,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_discord_privacy(
                            acc.clone(),
                            move |p| {
                                if is_explicit {
                                    p.explicit_content_filter = level;
                                } else {
                                    p.dm_spam_filter = level;
                                }
                            },
                            cx,
                        );
                    })),
                );
            }
            box_div = box_div.child(rule()).child(row_wrap(title, sub, seg.into_any_element()));
        }
        // Data & personalisation settings stay in Discord.
        box_div = box_div.child(rule()).child(row_wrap(
            "Data & Privacy",
            "Personalisation, analytics, and data export stay in Discord.",
            button(("dc-priv-open", ix), ButtonKind::Ghost, "Open in Discord")
                .on_click(|_, _, cx| cx.open_url("https://discord.com/channels/@me"))
                .into_any_element(),
        ));
        box_div
    }

    fn load_telegram_folders(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.tg_folders_requested.insert(account.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    match crate::client::ask(&protocol::Request::Folders { network: Network::Telegram }) {
                        Ok(protocol::Response::Folders { folders }) => {
                            let mut mine: Vec<_> = folders.into_iter().filter(|f| f.key.account() == acc).collect();
                            mine.sort_by_key(|f| f.position);
                            Some(mine)
                        }
                        _ => None,
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(folders) = loaded {
                    this.tg_folders.insert(account, folders);
                    cx.notify();
                } else {
                    this.tg_folders_requested.remove(&account);
                }
            });
        })
        .detach();
    }

    fn telegram_folders_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_telegram_folders(account.key.clone(), cx);
        let folders = self.tg_folders.get(&account.key).cloned().unwrap_or_default();
        let mut list = div().flex().flex_col().gap(px(6.));
        for (f_ix, folder) in folders.iter().enumerate() {
            let acc_up = account.key.clone();
            let acc_down = account.key.clone();
            let acc_del = account.key.clone();
            let fid = folder.key.id.clone();
            let order_ids: Vec<String> = folders.iter().map(|f| f.key.id.clone()).collect();
            let up_btn = (f_ix > 0).then(|| {
                let mut next = order_ids.clone();
                next.swap(f_ix - 1, f_ix);
                button(("tg-folder-up", ix * 100 + f_ix), ButtonKind::Ghost, "↑").on_click(cx.listener(
                    move |this, _, _, cx| {
                        let req = protocol::Request::FoldersReorder { account: acc_up.clone(), order: next.clone() };
                        let acc = acc_up.clone();
                        this.off_thread(cx, move || crate::client::ask(&req), move |this, _, cx| {
                            this.tg_folders_requested.remove(&acc);
                            this.load_telegram_folders(acc, cx);
                        });
                    },
                ))
            });
            let down_btn = (f_ix + 1 < folders.len()).then(|| {
                let mut next = order_ids.clone();
                next.swap(f_ix, f_ix + 1);
                button(("tg-folder-down", ix * 100 + f_ix), ButtonKind::Ghost, "↓").on_click(cx.listener(
                    move |this, _, _, cx| {
                        let req = protocol::Request::FoldersReorder { account: acc_down.clone(), order: next.clone() };
                        let acc = acc_down.clone();
                        this.off_thread(cx, move || crate::client::ask(&req), move |this, _, cx| {
                            this.tg_folders_requested.remove(&acc);
                            this.load_telegram_folders(acc, cx);
                        });
                    },
                ))
            });
            let del_btn = button(("tg-folder-del", ix * 100 + f_ix), ButtonKind::Ghost, "Delete").on_click(cx.listener(
                move |this, _, _, cx| {
                    let req = protocol::Request::FolderDelete { account: acc_del.clone(), folder_id: fid.clone() };
                    let acc = acc_del.clone();
                    this.off_thread(cx, move || crate::client::ask(&req), move |this, _, cx| {
                        this.tg_folders_requested.remove(&acc);
                        this.load_telegram_folders(acc, cx);
                    });
                },
            ));
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .child(div().type_ui().text_color(theme::text()).truncate().child(folder.name.clone()))
                    .child(div().flex().items_center().gap(px(4.)).children(up_btn).children(down_btn).child(del_btn)),
            );
        }
        let acc_create = account.key.clone();
        let name_input = self
            .tg_folder_name
            .entry(account.key.clone())
            .or_insert_with(|| cx.new(|cx| crate::gui::input::TextInput::new("New folder name", cx)))
            .clone();
        let create_row = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_1()
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::field())
                    .border_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .child(name_input.clone()),
            )
            .child(button(("tg-folder-add", ix), ButtonKind::Secondary, "Create folder").on_click(cx.listener(
                move |this, _, _, cx| {
                    let name = this
                        .tg_folder_name
                        .get(&acc_create)
                        .map(|input| input.read(cx).text().trim().to_owned())
                        .unwrap_or_default();
                    if name.is_empty() {
                        this.tg_folder_error.insert(acc_create.clone(), "Give the folder a name.".to_owned());
                        cx.notify();
                        return;
                    }
                    this.tg_folder_error.remove(&acc_create);
                    let spec = protocol::FolderSpec {
                        name,
                        contacts: true,
                        groups: true,
                        ..protocol::FolderSpec::default()
                    };
                    let req = protocol::Request::FolderSave { account: acc_create.clone(), folder: spec };
                    let acc = acc_create.clone();
                    this.off_thread(cx, move || crate::client::ask(&req), move |this, answer, cx| {
                        match answer {
                            Ok(protocol::Response::Error { detail, .. }) => {
                                this.tg_folder_error.insert(acc, detail);
                            }
                            Ok(_) => {
                                if let Some(input) = this.tg_folder_name.get(&acc).cloned() {
                                    input.update(cx, |input, cx| input.clear(cx));
                                }
                                this.tg_folders_requested.remove(&acc);
                                this.load_telegram_folders(acc, cx);
                            }
                            Err(err) => {
                                this.tg_folder_error.insert(acc, err.to_string());
                            }
                        }
                        cx.notify();
                    });
                },
            )));
        div().child(rule()).child(
            div()
                .px(px(12.))
                .py(px(8.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().type_ui().text_color(theme::text()).child("Chat folders"))
                .child(list)
                .child(create_row)
                .children(self.tg_folder_error.get(&account.key).map(|err| div().type_small().text_color(theme::danger()).child(err.clone()))),
        )
    }

    fn load_telegram_download(&mut self, account: protocol::AccountKey, cx: &mut Context<Self>) {
        if !self.tg_download_requested.insert(account.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move { crate::client::ask(&protocol::Request::GetAutoDownload { account: acc }) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(protocol::Response::AutoDownload { settings }) = loaded {
                    this.tg_download.insert(account, settings);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_telegram_download(&mut self, account: protocol::AccountKey, settings: protocol::AutoDownload, cx: &mut Context<Self>) {
        let Some(previous) = self.tg_download.get(&account).cloned() else {
            return;
        };
        self.tg_download.insert(account.clone(), settings.clone());
        cx.notify();
        let restore = previous.clone();
        cx.spawn(async move |this, cx| {
            let acc = account.clone();
            let sent = settings.clone();
            let answer = cx
                .background_executor()
                .spawn(async move { crate::client::ask(&protocol::Request::SetAutoDownload { account: acc, settings: sent }) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(protocol::Response::AutoDownload { settings }) => {
                        this.tg_download.insert(account, settings);
                    }
                    _ => {
                        this.tg_download.insert(account, restore.clone());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn telegram_download_rows(&mut self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        self.load_telegram_download(account.key.clone(), cx);
        let Some(settings) = self.tg_download.get(&account.key).cloned() else {
            return div().px(px(12.)).py(px(8.)).type_small().text_color(theme::text_faint()).child("Loading automatic downloads…");
        };
        let caps: [(&str, i64); 4] = [("Off", 0), ("1 MB", 1 << 20), ("10 MB", 10 << 20), ("100 MB", 100 << 20)];
        let networks: [(&str, usize); 3] = [("Roaming", 0), ("Mobile", 1), ("Wi-Fi", 2)];
        let mut box_div = div().flex().flex_col().child(rule()).child(
            div()
                .px(px(12.))
                .pt(px(10.))
                .pb(px(4.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().text_color(theme::text()).child("Automatic downloads"))
                .child(div().type_small().text_color(theme::text_soft()).child("Full photos, videos and files stay on Telegram until you open them, unless a chat kind is on and the file is under that network's limit. This device follows the Wi-Fi limit.")),
        );
        let switches: [(&str, bool); 4] = [
            ("Fetch full photos", settings.fetch_full_photos),
            ("Private chats", settings.private_chats),
            ("Groups", settings.groups),
            ("Channels", settings.channels),
        ];
        for (switch_ix, (label, on)) in switches.into_iter().enumerate() {
            let base = settings.clone();
            let acc = account.key.clone();
            box_div = box_div.child(rule()).child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().type_ui().text_color(theme::text()).child(label))
                    .child(super::controls::switch(("tg-dl-sw", ix * 10 + switch_ix), label, on).on_click(cx.listener(move |this, _, _, cx| {
                        let mut next = base.clone();
                        match switch_ix {
                            0 => next.fetch_full_photos = !on,
                            1 => next.private_chats = !on,
                            2 => next.groups = !on,
                            _ => next.channels = !on,
                        }
                        this.set_telegram_download(acc.clone(), next, cx);
                    }))),
            );
        }
        for (net_ix, (name, slot)) in networks.into_iter().enumerate() {
            let preset = match slot {
                0 => &settings.roaming,
                1 => &settings.mobile,
                _ => &settings.wifi,
            };
            let mut seg = div().flex().items_center().gap(px(4.));
            for (cap_ix, (label, bytes)) in caps.into_iter().enumerate() {
                let active = if bytes == 0 {
                    preset.disabled || preset.photo_size_max <= 0
                } else {
                    !preset.disabled && preset.photo_size_max == bytes
                };
                let acc = account.key.clone();
                let base = settings.clone();
                seg = seg.child(
                    button(("tg-dl-cap", ix * 100 + net_ix * 10 + cap_ix), if active { ButtonKind::Primary } else { ButtonKind::Secondary }, label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut next = base.clone();
                            let preset = match slot {
                                0 => &mut next.roaming,
                                1 => &mut next.mobile,
                                _ => &mut next.wifi,
                            };
                            if bytes <= 0 {
                                *preset = protocol::AutoDownloadPreset::default();
                            } else {
                                preset.disabled = false;
                                preset.photo_size_max = bytes;
                                preset.video_size_max = bytes.saturating_mul(10);
                                preset.file_size_max = bytes;
                            }
                            this.set_telegram_download(acc.clone(), next, cx);
                        })),
                );
            }
            box_div = box_div.child(rule()).child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().type_ui().text_color(theme::text()).child(name))
                    .child(seg),
            );
        }
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
        let mut lines = vec![
            crate::host::sign_out_body(network),
            format!("Nothing is deleted on {network}. Your other accounts are not touched."),
        ];
        if account.key.network == Network::Teams {
            lines.push(
                "This removes TinyClient's push registration at Microsoft. To end Microsoft sessions across devices, visit myaccount.microsoft.com.".to_owned(),
            );
        }
        super::modal_panel(
            format!("Sign out of {name}?"),
            lines,
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
        if can.profile_accent {
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(field_box("Accent colour", self.profile_accent.clone()))
                    .child(div().type_small().text_color(theme::text_faint()).child("Hex, like 5865F2. Leave blank to keep the current colour.")),
            );
        }
        if can.profile_username {
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(field_box("Username", self.profile_username.clone()))
                    .child(
                        div()
                            .type_small()
                            .text_color(theme::text_faint())
                            .child("Changing or clearing your @username releases your old link immediately."),
                    ),
            );
        }
        if can.profile_birthday {
            fields = fields.child(field_box("Birthday", self.profile_birthday.clone()));
        }
        if can.rich_status && key.network == Network::Telegram {
            fields = fields.child(field_box("Emoji status (Premium)", self.profile_emoji_status.clone()));
        }
        if can.profile_avatar {
            let avatar_key = key.clone();
            let clear_key = key.clone();
            fields = fields.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_small().text_color(theme::text_soft()).child("Profile picture"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
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
                            )))
                            .child(button("profile-clear-avatar", ButtonKind::Ghost, "Remove").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let req = protocol::Request::SetAvatar { account: clear_key.clone(), path: String::new() };
                                    this.off_thread(cx, move || crate::client::ask(&req), |_, _, cx| cx.notify());
                                },
                            ))),
                    ),
            );
        }
        if can.profile_banner {
            let banner_key = key.clone();
            fields = fields.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_small().text_color(theme::text_soft()).child("Profile banner"))
                    .child(button("profile-pick-banner", ButtonKind::Secondary, "Choose banner…").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let banner_key = banner_key.clone();
                            let asked = cx.prompt_for_paths(gpui::PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: Some("Choose profile banner".into()),
                            });
                            cx.spawn(async move |this, cx| {
                                if let Ok(Ok(Some(paths))) = asked.await {
                                    if let Some(path) = paths.into_iter().next() {
                                        let path_str = path.to_string_lossy().into_owned();
                                        let req = protocol::Request::SetProfile {
                                            account: banner_key,
                                            name: None,
                                            about: None,
                                            pronouns: None,
                                            emoji: None,
                                            username: None,
                                            photo: false,
                                            accent: None,
                                            banner: Some(path_str),
                                            space: None,
                                            birthday: None,
                                        };
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
        if can.profile_server {
            let account_key = key.clone();
            let guilds: Vec<(protocol::Key, String)> = self
                .shell
                .upgrade()
                .map(|shell| {
                    shell
                        .read(cx)
                        .spaces
                        .iter()
                        .filter(|space| space.key.account() == account_key)
                        .map(|space| (space.key.clone(), space.name.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let selected = self.profile_space.clone().filter(|space| space.account() == key);
            let mut section = div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .pt(px(8.))
                .child(div().type_small().text_color(theme::text_soft()).child("Server profile"));
            if guilds.is_empty() {
                section = section.child(div().type_small().text_color(theme::text_faint()).child("No servers on this account."));
            } else {
                let mut list = div().id("profile-server-list").max_h(px(140.)).overflow_y_scroll().flex().flex_col().gap(px(2.));
                for (ix, (guild, name)) in guilds.iter().enumerate() {
                    let on = selected.as_ref() == Some(guild);
                    let pick = guild.clone();
                    list = list.child(
                        div()
                            .id(("profile-server", ix))
                            .px(px(8.))
                            .h(px(28.))
                            .rounded(px(theme::R_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .when(on, |row| row.bg(theme::selected()))
                            .hover(|row| row.bg(theme::hover()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.profile_space = Some(pick.clone());
                                cx.notify();
                            }))
                            .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(name.clone())),
                    );
                }
                section = section.child(list);
            }
            if let Some(guild) = selected.clone() {
                let banner_account = key.clone();
                let banner_space = guild.clone();
                section = section
                    .child(field_box("Server about", self.profile_server_about.clone()))
                    .child(field_box("Server pronouns", self.profile_server_pronouns.clone()))
                    .child(field_box("Server accent colour", self.profile_server_accent.clone()))
                    .child(div().type_small().text_color(theme::text_faint()).child("Blank fields stay as they are. Hex, like 5865F2."))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().type_small().text_color(theme::text_soft()).child("Server banner"))
                            .child(button("profile-server-banner", ButtonKind::Secondary, "Choose banner…").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let account = banner_account.clone();
                                    let space = banner_space.clone();
                                    let asked = cx.prompt_for_paths(gpui::PathPromptOptions {
                                        files: true,
                                        directories: false,
                                        multiple: false,
                                        prompt: Some("Choose server banner".into()),
                                    });
                                    cx.spawn(async move |this, cx| {
                                        if let Ok(Ok(Some(paths))) = asked.await {
                                            if let Some(path) = paths.into_iter().next() {
                                                let req = protocol::Request::SetProfile {
                                                    account,
                                                    name: None,
                                                    about: None,
                                                    pronouns: None,
                                                    emoji: None,
                                                    username: None,
                                                    photo: false,
                                                    accent: None,
                                                    banner: Some(path.to_string_lossy().into_owned()),
                                                    space: Some(space),
                                                    birthday: None,
                                                };
                                                let _ = cx.background_executor().spawn(async move { crate::client::ask(&req) }).await;
                                                let _ = this.update(cx, |_, cx| cx.notify());
                                            }
                                        }
                                    })
                                    .detach();
                                },
                            ))),
                    );
                let save_account = key.clone();
                let save_space = guild;
                section = section.child(
                    div().flex().justify_end().child(
                        button("profile-save-server", ButtonKind::Primary, "Save server profile").on_click(cx.listener(move |this, _, _, cx| {
                            let about = this.profile_server_about.read(cx).text().trim().to_owned();
                            let pronouns = this.profile_server_pronouns.read(cx).text().trim().to_owned();
                            let accent = accent_u32(&this.profile_server_accent.read(cx).text());
                            if about.is_empty() && pronouns.is_empty() && accent.is_none() {
                                return;
                            }
                            let req = protocol::Request::SetProfile {
                                account: save_account.clone(),
                                name: None,
                                about: (!about.is_empty()).then_some(about),
                                pronouns: (!pronouns.is_empty()).then_some(pronouns),
                                emoji: None,
                                username: None,
                                photo: false,
                                accent,
                                banner: None,
                                space: Some(save_space.clone()),
                                birthday: None,
                            };
                            this.off_thread(cx, move || crate::client::ask(&req), |_, _, cx| cx.notify());
                        })),
                    ),
                );
            }
            fields = fields.child(section);
        }
        if account.key.network == Network::Discord {
            fields = fields.child(
                div()
                    .pt(px(4.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_small().text_color(theme::text_faint()).child("Username & email"))
                    .child(
                        button("profile-open-discord", ButtonKind::Ghost, "Change in Discord ↗").on_click(|_, _, cx| {
                            cx.open_url("https://discord.com/channels/@me");
                        }),
                    ),
            );
        }
        let save_key = key.clone();
        let save = button("profile-save", ButtonKind::Primary, "Save").on_click(cx.listener(move |this, _, _, cx| {
            let name = can.profile_name.then(|| this.profile_name.read(cx).text().trim().to_owned()).filter(|s| !s.is_empty());
            let about = can.profile_about.then(|| this.profile_about.read(cx).text().trim().to_owned());
            let pronouns = can.profile_pronouns.then(|| this.profile_pronouns.read(cx).text().trim().to_owned());
            let accent = can.profile_accent.then(|| accent_u32(&this.profile_accent.read(cx).text())).flatten();
            let username = can
                .profile_username
                .then(|| this.profile_username.read(cx).text().trim().to_owned())
                .filter(|s| !s.is_empty());
            let birthday = can
                .profile_birthday
                .then(|| this.profile_birthday.read(cx).text().trim().to_owned())
                .filter(|s| !s.is_empty());
            let emoji = (can.rich_status && save_key.network == Network::Telegram)
                .then(|| this.profile_emoji_status.read(cx).text().trim().to_owned())
                .filter(|s| !s.is_empty());
            let req = protocol::Request::SetProfile {
                account: save_key.clone(),
                name,
                about,
                pronouns,
                emoji,
                username,
                birthday,
                photo: false,
                accent,
                banner: None,
                space: None,
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

/// Six hex digits, with or without a leading `#`. Empty and anything else are left unset.
fn accent_u32(text: &str) -> Option<u32> {
    let text = text.trim().trim_start_matches('#');
    if text.len() != 6 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(text, 16).ok().map(|color| color & 0x00FF_FFFF)
}
