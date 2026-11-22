//! Discord account management. Lists are scoped to the visible account and opened by the user.

use super::controls::{row, rule};
use super::SettingsWindow;
use crate::gui::components::{button, ButtonKind};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, SharedString};
use protocol::{
    AccountKey, AccountStatus, DiscordAccountEntryId, DiscordAccountList, Link, Request, Response,
};

const DISCORD_ACCOUNT: &str = "https://discord.com/channels/@me";
const DISCORD_CONNECTIONS: &str = "https://discord.com/settings/connections";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Devices,
    Apps,
    Connections,
}

impl List {
    fn label(self) -> &'static str {
        match self {
            List::Devices => "Devices",
            List::Apps => "Authorized apps",
            List::Connections => "Connections",
        }
    }

    fn open_label(self) -> &'static str {
        match self {
            List::Devices => "View devices",
            List::Apps => "View apps",
            List::Connections => "View connections",
        }
    }

    fn empty_label(self) -> &'static str {
        match self {
            List::Devices => "No devices listed.",
            List::Apps => "No authorized apps.",
            List::Connections => "No connections.",
        }
    }

    fn request(self, account: AccountKey) -> Request {
        match self {
            List::Devices => Request::ActiveSessions { account },
            List::Apps => Request::DiscordAccountEntries { account, list: DiscordAccountList::AuthorizedApps },
            List::Connections => Request::DiscordAccountEntries { account, list: DiscordAccountList::Connections },
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Action {
    SignOut(String),
    SignOutOthers,
    Remove(DiscordAccountEntryId),
}

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Action::SignOut(_) => "Sign out device",
            Action::SignOutOthers => "Sign out other devices",
            Action::Remove(DiscordAccountEntryId::AuthorizedApp { .. }) => "Revoke access",
            Action::Remove(DiscordAccountEntryId::Connection { .. }) => "Disconnect",
        }
    }

    fn confirm_label(&self) -> &'static str {
        match self {
            Action::SignOut(_) | Action::SignOutOthers => "Confirm sign-out",
            Action::Remove(DiscordAccountEntryId::AuthorizedApp { .. }) => "Confirm revoke",
            Action::Remove(DiscordAccountEntryId::Connection { .. }) => "Confirm disconnect",
        }
    }

    fn request(&self, account: AccountKey) -> Request {
        match self {
            Action::SignOut(session_id) => Request::EndSession { account, session_id: session_id.clone() },
            Action::SignOutOthers => Request::EndOtherDiscordSessions { account },
            Action::Remove(target) => Request::RemoveDiscordAccountEntry { account, target: target.clone() },
        }
    }
}

struct Entry {
    title: String,
    detail: String,
    current: bool,
    action: Action,
}

#[derive(Default)]
pub(super) struct AccountPanel {
    open: Option<List>,
    entries: Vec<Entry>,
    pending: bool,
    loaded: bool,
    confirm: Option<Action>,
    error: Option<String>,
}

fn answer_error(answer: std::io::Result<Response>) -> String {
    match answer {
        Ok(Response::Error { detail, .. }) => detail,
        Ok(_) => "Discord account settings are unavailable.".to_owned(),
        Err(err) => crate::client::trouble_words("Discord account settings", &err),
    }
}

fn entries(list: List, answer: std::io::Result<Response>) -> Result<Vec<Entry>, String> {
    match (list, answer) {
        (List::Devices, Ok(Response::ActiveSessions { sessions })) => Ok(sessions.into_iter().map(|session| {
            let title = if session.app.is_empty() { "Unknown client".to_owned() } else { session.app };
            let last = (session.last_active > 0).then(|| {
                format!("Last used {} at {}", crate::gui::time::date(session.last_active), crate::gui::time::clock(session.last_active))
            });
            let detail = [session.platform, session.location, last.unwrap_or_default()]
                .into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
            Entry { title, detail, current: session.current, action: Action::SignOut(session.id) }
        }).collect()),
        (List::Apps | List::Connections, Ok(Response::DiscordAccountEntries { entries })) => {
            Ok(entries.into_iter().map(|entry| {
                let detail = match &entry.target {
                    DiscordAccountEntryId::Connection { provider, .. } => provider.clone(),
                    DiscordAccountEntryId::AuthorizedApp { .. } => String::new(),
                };
                Entry { title: entry.name, detail, current: false, action: Action::Remove(entry.target) }
            }).collect())
        }
        (_, answer) => Err(answer_error(answer)),
    }
}

fn link_row(id: (&'static str, usize), label: &'static str, detail: Option<SharedString>, url: &'static str) -> Div {
    row(label, detail, button(id, ButtonKind::Secondary, "Open in Discord ↗").on_click(move |_, _, cx| cx.open_url(url)))
}

impl SettingsWindow {
    fn load_discord_list(&mut self, account: AccountKey, list: List, cx: &mut Context<Self>) {
        let panel = self.discord_account.entry(account.clone()).or_default();
        if panel.pending {
            return;
        }
        *panel = AccountPanel { open: Some(list), pending: true, ..AccountPanel::default() };
        let request = list.request(account.clone());
        self.off_thread(cx, move || crate::client::ask(&request), move |this, answer, cx| {
            if let Some(panel) = this.discord_account.get_mut(&account) {
                panel.pending = false;
                match entries(list, answer) {
                    Ok(entries) => {
                        panel.entries = entries;
                        panel.loaded = true;
                    }
                    Err(error) => panel.error = Some(error),
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    fn change_discord_account(&mut self, account: AccountKey, action: Action, cx: &mut Context<Self>) {
        let Some(panel) = self.discord_account.get_mut(&account) else { return };
        if panel.pending || panel.confirm.as_ref() != Some(&action) {
            return;
        }
        panel.pending = true;
        panel.confirm = None;
        panel.error = None;
        let request = action.request(account.clone());
        self.off_thread(cx, move || crate::client::ask(&request), move |this, answer, cx| {
            if let Some(panel) = this.discord_account.get_mut(&account) {
                panel.pending = false;
                match answer {
                    Ok(Response::Ok) if action == Action::SignOutOthers => panel.entries.retain(|entry| entry.current),
                    Ok(Response::Ok) => panel.entries.retain(|entry| entry.action != action),
                    other => panel.error = Some(answer_error(other)),
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn discord_account_rows(&self, account: &AccountStatus, ix: usize, cx: &mut Context<Self>) -> Div {
        let mut rows = div().child(rule()).child(link_row(
            ("discord-account", ix),
            "Account on Discord",
            Some("Email, password, phone, 2FA, passkeys and account removal".into()),
            DISCORD_ACCOUNT,
        )).child(rule()).child(link_row(
            ("discord-family-centre", ix),
            "Family Centre",
            None,
            "https://discord.com/feature/family-center",
        ));
        let plan = account.can.discord_plan.filter(|_| account.link == Link::Connected);
        rows = rows.child(rule()).child(link_row(
            ("discord-nitro", ix),
            "Nitro & billing",
            plan.map(|plan| plan.label().into()),
            "https://discord.com/settings/subscriptions",
        )).child(rule()).child(link_row(
            ("discord-shop", ix),
            "Shop",
            None,
            "https://discord.com/shop",
        )).child(rule()).child(link_row(
            ("discord-quests", ix),
            "Quests",
            None,
            "https://discord.com/discovery/quests",
        ));
        if account.link != Link::Connected {
            return rows;
        }
        let panel = self.discord_account.get(&account.key);
        for (index, list) in [List::Devices, List::Apps, List::Connections].into_iter().enumerate() {
            if list == List::Devices && !account.can.active_sessions {
                continue;
            }
            let open = panel.filter(|panel| panel.open == Some(list));
            let key = account.key.clone();
            let busy = panel.is_some_and(|panel| panel.pending);
            let control = if busy {
                div().into_any_element()
            } else {
                let label = if open.is_some() { "Refresh" } else { list.open_label() };
                button(("discord-account-list", ix * 3 + index), ButtonKind::Secondary, label)
                    .on_click(cx.listener(move |this, _, _, cx| this.load_discord_list(key.clone(), list, cx)))
                    .into_any_element()
            };
            rows = rows.child(rule()).child(row(list.label(), None, control));
            if let Some(panel) = open {
                rows = rows.child(self.discord_list(&account.key, ix, list, panel, cx));
            }
        }
        rows
    }

    fn discord_list(&self, account: &AccountKey, ix: usize, list: List, panel: &AccountPanel, cx: &mut Context<Self>) -> Div {
        let mut rows = div();
        if let Some(error) = &panel.error {
            rows = rows.child(
                div().px(px(12.)).py(px(6.)).type_small().text_color(theme::danger()).child(error.clone()),
            );
        }
        if panel.pending {
            let status = if panel.loaded { "Updating Discord…" } else { "Loading from Discord…" };
            rows = rows.child(row(status, None, div()));
        }
        if panel.loaded && panel.entries.is_empty() {
            rows = rows.child(row(list.empty_label(), None, div()));
        }
        let identified = panel.entries.iter().any(|entry| entry.current);
        if list == List::Devices && !panel.entries.is_empty() && !identified {
            rows = rows.child(row("Manage sign-outs in Discord if this device cannot be identified.", None, div()));
        }
        for (index, entry) in panel.entries.iter().enumerate() {
            let control = if entry.current {
                div().type_small().text_color(theme::accent_text()).child("This device").into_any_element()
            } else if panel.pending || (list == List::Devices && !identified) {
                div().into_any_element()
            } else {
                self.discord_account_action(account, ix, Some(index), &entry.action, panel, cx)
            };
            let detail = (!entry.detail.is_empty()).then(|| entry.detail.clone().into());
            rows = rows.child(row(entry.title.clone(), detail, control));
        }
        if list == List::Devices && identified && panel.entries.len() > 1 && !panel.pending {
            rows = rows.child(row(
                "Other devices",
                Some("Keep this device signed in.".into()),
                self.discord_account_action(account, ix, None, &Action::SignOutOthers, panel, cx),
            ));
        }
        if list == List::Connections {
            rows = rows.child(link_row(("discord-add-connection", ix), "Add a connection", None, DISCORD_CONNECTIONS));
        }
        rows
    }

    fn discord_account_action(
        &self, account: &AccountKey, ix: usize, index: Option<usize>, action: &Action, panel: &AccountPanel,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let account = account.clone();
        let action = action.clone();
        let id = |kind| SharedString::from(format!("discord-account-{ix}-{index:?}-{kind}"));
        if panel.confirm.as_ref() == Some(&action) {
            let cancel_account = account.clone();
            div().flex().items_center().gap(px(4.))
                .child(button(id("cancel"), ButtonKind::Ghost, "Cancel").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(panel) = this.discord_account.get_mut(&cancel_account) {
                        panel.confirm = None;
                    }
                    cx.notify();
                })))
                .child(button(id("confirm"), ButtonKind::Danger, action.confirm_label()).on_click(cx.listener(move |this, _, _, cx| {
                    this.change_discord_account(account.clone(), action.clone(), cx);
                })))
                .into_any_element()
        } else {
            button(id("start"), ButtonKind::Ghost, action.label()).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(panel) = this.discord_account.get_mut(&account) {
                    panel.confirm = Some(action.clone());
                    panel.error = None;
                }
                cx.notify();
            })).into_any_element()
        }
    }
}
