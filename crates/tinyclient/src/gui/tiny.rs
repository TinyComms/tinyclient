//! The Tiny server window. People are the sidebar. The pane is the invite, or the one person
//! you picked. A control the account cannot use is absent, never disabled.

use super::components::{button, button_disabled, glyph, list_row, pane_title, section_header, segment, segmented, Avatar, ButtonKind};
use super::layout::{drags_window, TOP_BAR};
use super::sidebar::Mode;
use super::state::{Shell, Snapshot};
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, MouseButton, Window};
use protocol::{AccountKey, Key, Link, Network, Request, Response};
use std::io;

/// What the server window is holding. The invite token stays until Done and is never logged.
pub struct State {
    seen: bool,
    account: Option<AccountKey>,
    load_epoch: u64,
    action_epoch: u64,
    loading: bool,
    community: Option<protocol::TinyCommunity>,
    error: Option<String>,
    /// `"member"` or `"admin"`. Never owner.
    invite_role: String,
    token: Option<String>,
    busy: bool,
    confirm: Option<String>,
    /// A person on show. None is the invite.
    picked: Option<String>,
}

impl Default for State {
    fn default() -> State {
        State {
            seen: false,
            account: None,
            load_epoch: 0,
            action_epoch: 0,
            loading: false,
            community: None,
            error: None,
            invite_role: "member".into(),
            token: None,
            busy: false,
            confirm: None,
            picked: None,
        }
    }
}

struct Member {
    user_id: String,
    name: String,
    email: String,
    role: String,
    make_admin: bool,
    make_member: bool,
    make_owner: bool,
    remove: bool,
}

struct OpenInvite {
    role: String,
    expires: String,
}

struct Ready {
    error: Option<String>,
    token: Option<String>,
    invite_role: String,
    can_invite: bool,
    busy: bool,
    confirm: Option<String>,
    members: Vec<Member>,
    invites: Vec<OpenInvite>,
    /// `None` when this account cannot read the audit.
    audit: Option<Vec<String>>,
}

enum Page {
    Loading,
    Failed(String),
    Ready(Ready),
}

fn connected(snapshot: &Snapshot) -> Option<AccountKey> {
    snapshot
        .accounts
        .iter()
        .find(|status| status.key.network == Network::Tiny && status.link == Link::Connected)
        .map(|status| status.key.clone())
}

fn same_role(role: &str, name: &str) -> bool {
    role.eq_ignore_ascii_case(name)
}

fn known_role(role: &str) -> bool {
    same_role(role, "owner") || same_role(role, "admin") || same_role(role, "member")
}

/// A role button is drawn only when that change would be legal.
fn role_change_legal(caller: &str, target: &str, next: &str, owners: usize) -> bool {
    if same_role(target, next) || !known_role(next) {
        return false;
    }
    // The only owner cannot be demoted.
    if same_role(target, "owner") && owners <= 1 {
        return false;
    }
    // Changing an owner, or granting owner, is the owner's act alone.
    if same_role(target, "owner") || same_role(next, "owner") {
        return same_role(caller, "owner");
    }
    if same_role(caller, "owner") {
        return true;
    }
    // An admin may promote a member, and does not demote another admin.
    same_role(caller, "admin") && same_role(target, "member") && same_role(next, "admin")
}

fn remove_legal(caller: &str, target: &str, owners: usize) -> bool {
    if same_role(target, "owner") && owners <= 1 {
        return false;
    }
    same_role(caller, "owner") || (same_role(caller, "admin") && same_role(target, "member"))
}

fn can_make(caller: &str, target: &str, next: &str, owners: usize) -> bool {
    protocol::tiny_can(caller, "community.members.role") && role_change_legal(caller, target, next, owners)
}

fn can_remove(caller: &str, target: &str, owners: usize) -> bool {
    protocol::tiny_can(caller, "community.members.remove") && remove_legal(caller, target, owners)
}

fn role_word(role: &str) -> String {
    if same_role(role, "owner") {
        "Owner".to_owned()
    } else if same_role(role, "admin") {
        "Admin".to_owned()
    } else if same_role(role, "member") {
        "Member".to_owned()
    } else {
        role.to_owned()
    }
}

fn person_name(display: &str, email: &str) -> (String, String) {
    if !display.is_empty() {
        (display.to_owned(), email.to_owned())
    } else if !email.is_empty() {
        (email.to_owned(), String::new())
    } else {
        ("Someone".to_owned(), String::new())
    }
}

impl Shell {
    /// Loads the server when the window is actually showing it, and again the next time it is.
    pub(super) fn note_tiny_page(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_shown() else {
            self.tiny_state.seen = false;
            return;
        };
        if self.tiny_state.seen && self.tiny_state.account.as_ref() == Some(&account) {
            return;
        }
        self.tiny_state.seen = true;
        self.fetch(account, None, cx);
    }

    pub(super) fn tiny_open(&self) -> bool {
        self.tiny_state.seen
    }

    fn tiny_shown(&self) -> Option<AccountKey> {
        if self.shown().empty_state().is_some() {
            return None;
        }
        if !matches!(self.sidebar_state.mode, Mode::Network(Network::Tiny)) {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.open.is_some() {
            return None;
        }
        connected(snapshot)
    }

    pub(super) fn tiny_page(&self, people_beside: bool, window: &Window, cx: &mut Context<Self>) -> Div {
        let page = self.page();
        let me = self.tiny_state.account.as_ref().map(|key| key.account.as_str());
        let (title, body) = match &page {
            Page::Loading => ("Tiny server".to_owned(), note_line("Loading this server.", false).into_any_element()),
            Page::Failed(detail) => ("Tiny server".to_owned(), note_line(detail, true).into_any_element()),
            Page::Ready(ready) => {
                let shown = shown_id(self.tiny_state.picked.as_deref(), ready.can_invite, &ready.members);
                let title = shown
                    .and_then(|id| ready.members.iter().find(|member| member.user_id == id))
                    .map(|member| member.name.clone())
                    .unwrap_or_else(|| "Invite".to_owned());
                (title, ready_view(ready, shown, me, people_beside, cx))
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                drags_window(div())
                    .h(px(TOP_BAR))
                    .flex_none()
                    .px(px(theme::PANE_PAD))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.strip_controls(window, cx))
                    .child(pane_title(title)),
            )
            .child(body)
    }

    fn page(&self) -> Page {
        let state = &self.tiny_state;
        if state.loading && state.community.is_none() {
            return Page::Loading;
        }
        let Some(community) = state.community.as_ref() else {
            return match state.error.clone() {
                Some(detail) => Page::Failed(detail),
                None => Page::Loading,
            };
        };
        let caller = community.role.as_str();
        let owners = community.members.iter().filter(|member| same_role(&member.role, "owner")).count();
        let can_invite = protocol::tiny_can(caller, "community.invites.create");
        let members = if protocol::tiny_can(caller, "community.members.read") {
            community
                .members
                .iter()
                .map(|member| {
                    let (name, email) = person_name(&member.display_name, &member.email);
                    let role = member.role.as_str();
                    Member {
                        user_id: member.user_id.clone(),
                        name,
                        email,
                        role: role_word(role),
                        make_admin: can_make(caller, role, "admin", owners),
                        make_member: can_make(caller, role, "member", owners),
                        make_owner: can_make(caller, role, "owner", owners),
                        remove: can_remove(caller, role, owners),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let invites = if protocol::tiny_can(caller, "community.invites.read") {
            community
                .invites
                .iter()
                .filter(|invite| invite.accepted_at.is_none())
                .map(|invite| OpenInvite { role: role_word(&invite.role), expires: invite.expires_at.to_string() })
                .collect()
        } else {
            Vec::new()
        };
        let audit = protocol::tiny_can(caller, "community.audit.read").then(|| {
            community
                .audit
                .iter()
                .filter_map(|row| audit_line(&row.action, &row.detail, |id| members.iter().find(|member| member.user_id == id).map(|member| member.name.as_str())))
                .collect()
        });
        Page::Ready(Ready {
            error: state.error.clone().filter(|detail| !detail.is_empty()),
            token: state.token.clone(),
            invite_role: state.invite_role.clone(),
            can_invite,
            busy: state.busy,
            confirm: state.confirm.clone(),
            members,
            invites,
            audit,
        })
    }

    /// `clear_busy` is the action whose button stays quiet until this read returns.
    fn fetch(&mut self, account: AccountKey, clear_busy: Option<u64>, cx: &mut Context<Self>) {
        let switched = self.tiny_state.account.as_ref() != Some(&account);
        if switched {
            self.tiny_state.community = None;
            self.tiny_state.token = None;
            self.tiny_state.confirm = None;
            self.tiny_state.picked = None;
            self.tiny_state.error = None;
            self.tiny_state.invite_role = "member".into();
            self.tiny_state.action_epoch += 1;
            self.tiny_state.busy = false;
        }
        self.tiny_state.account = Some(account.clone());
        if self.tiny_state.community.is_none() {
            self.tiny_state.loading = true;
            self.tiny_state.error = None;
        }
        self.tiny_state.load_epoch += 1;
        let epoch = self.tiny_state.load_epoch;
        self.off_thread(cx, move || ask(&Request::TinyCommunity { account }), move |this, answer, cx| {
            if this.tiny_state.load_epoch != epoch {
                return;
            }
            this.tiny_state.loading = false;
            if let Some(action) = clear_busy {
                if this.tiny_state.action_epoch == action {
                    this.tiny_state.busy = false;
                }
            }
            match answer {
                Ok(Response::TinyCommunity { community }) => {
                    let confirm = this.tiny_state.confirm.clone();
                    if confirm.as_ref().is_some_and(|id| community.members.iter().all(|member| &member.user_id != id)) {
                        this.tiny_state.confirm = None;
                    }
                    if this.tiny_state.picked.as_ref().is_some_and(|id| community.members.iter().all(|member| &member.user_id != id)) {
                        this.tiny_state.picked = None;
                    }
                    this.tiny_state.error = None;
                    this.tiny_state.community = Some(community);
                }
                Ok(Response::Error { detail, .. }) => this.tiny_state.error = Some(detail),
                Ok(_) => this.tiny_state.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                Err(err) => this.tiny_state.error = Some(crate::client::trouble_words(&err)),
            }
            cx.notify();
        });
    }

    fn act(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_state.busy = true;
        self.tiny_state.error = None;
        self.tiny_state.action_epoch += 1;
        let epoch = self.tiny_state.action_epoch;
        cx.notify();
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| this.acted(epoch, account, answer, cx));
    }

    fn acted(&mut self, epoch: u64, account: AccountKey, answer: io::Result<Response>, cx: &mut Context<Self>) {
        if self.tiny_state.action_epoch != epoch {
            return;
        }
        if self.tiny_state.account.as_ref() != Some(&account) {
            self.tiny_state.busy = false;
            cx.notify();
            return;
        }
        match answer {
            Ok(Response::Ok) => {
                self.tiny_state.confirm = None;
                self.tiny_state.error = None;
                self.fetch(account, Some(epoch), cx);
                cx.notify();
            }
            Ok(Response::TinyCode { code }) => {
                self.tiny_state.token = Some(code.token);
                self.tiny_state.error = None;
                self.fetch(account, Some(epoch), cx);
                cx.notify();
            }
            Ok(Response::Error { detail, .. }) => {
                self.tiny_state.busy = false;
                self.tiny_state.error = Some(detail);
                cx.notify();
            }
            Ok(_) => {
                self.tiny_state.busy = false;
                self.tiny_state.error = Some(crate::client::OTHER_ANSWER.to_owned());
                cx.notify();
            }
            Err(err) => {
                self.tiny_state.busy = false;
                self.tiny_state.error = Some(crate::client::trouble_words(&err));
                cx.notify();
            }
        }
    }

    fn send_invite(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let role = if self.tiny_state.invite_role == "admin" { "admin" } else { "member" };
        self.act(Request::TinyInvite { account, role: role.to_owned() }, cx);
    }

    fn pick_role(&mut self, role: &'static str, cx: &mut Context<Self>) {
        if self.tiny_state.busy || (role != "member" && role != "admin") {
            return;
        }
        self.tiny_state.invite_role = role.to_owned();
        cx.notify();
    }

    fn send_role(&mut self, user_id: String, role: &'static str, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.act(Request::TinyRole { account, user_id, role: role.to_owned() }, cx);
    }

    fn ask_remove(&mut self, user_id: String, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        self.tiny_state.confirm = Some(user_id);
        cx.notify();
    }

    fn cancel_remove(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.confirm = None;
        cx.notify();
    }

    fn send_remove(&mut self, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_state.confirm = None;
        self.act(Request::TinyRemove { account, user_id }, cx);
    }

    fn copy_code(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.tiny_state.token.clone() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(token));
    }

    fn done_code(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.token = None;
        cx.notify();
    }

    fn pick_invite(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.picked = None;
        self.tiny_state.confirm = None;
        cx.notify();
    }

    fn pick_person(&mut self, user_id: String, cx: &mut Context<Self>) {
        self.tiny_state.picked = Some(user_id);
        self.tiny_state.confirm = None;
        cx.notify();
    }

    /// The sidebar's list: the invite, then the people. The pane draws the same list only when
    /// the sidebar is not beside it.
    pub(super) fn tiny_directory(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page() {
            Page::Loading => directory_note("Loading this server.", false),
            Page::Failed(detail) => directory_note(&detail, true),
            Page::Ready(ready) => {
                let shown = shown_id(self.tiny_state.picked.as_deref(), ready.can_invite, &ready.members);
                directory(&ready, shown, false, cx)
            }
        }
    }
}

fn note_line(words: &str, bad: bool) -> Div {
    let line = div().flex_1().px(px(theme::PANE_PAD)).pt(px(8.));
    if bad {
        line.type_small().text_color(theme::danger()).child(words.to_owned())
    } else {
        line.type_ui().text_color(theme::text_soft()).child(words.to_owned())
    }
}

/// Who the pane shows. None is the invite. Someone who cannot invite lands on the first person.
fn shown_id<'a>(picked: Option<&'a str>, can_invite: bool, members: &'a [Member]) -> Option<&'a str> {
    if let Some(id) = picked {
        if members.iter().any(|member| member.user_id == id) {
            return Some(id);
        }
    }
    if !can_invite {
        return members.first().map(|member| member.user_id.as_str());
    }
    None
}

fn ready_view(ready: &Ready, shown: Option<&str>, me: Option<&str>, people_beside: bool, cx: &mut Context<Shell>) -> AnyElement {
    let detail = match shown.and_then(|id| ready.members.iter().find(|member| member.user_id == id)) {
        Some(member) => person_view(member, me == Some(member.user_id.as_str()), ready.confirm.as_deref(), cx),
        None => server_view(ready, cx),
    };
    let column = div()
        .w_full()
        .pb(px(24.))
        .flex()
        .flex_col()
        .when(!people_beside, |column| column.child(directory(ready, shown, true, cx)))
        .child(detail);
    div().id("tiny-page").flex_1().min_h_0().overflow_y_scroll().child(column).into_any_element()
}

fn directory_note(words: &str, bad: bool) -> AnyElement {
    let line = div().px(px(theme::PANE_PAD)).pt(px(12.));
    let line = if bad { line.type_small().text_color(theme::danger()) } else { line.type_ui().text_color(theme::text_soft()) };
    line.child(words.to_owned()).into_any_element()
}

fn directory(ready: &Ready, shown: Option<&str>, in_pane: bool, cx: &mut Context<Shell>) -> AnyElement {
    let mut column = div().id(if in_pane { "tiny-people-pane" } else { "tiny-people" }).w_full().when(!in_pane, |column| column.size_full().overflow_y_scroll()).flex().flex_col();
    if ready.can_invite {
        let selected = shown.is_none();
        let content = div()
            .gap(px(8.))
            .child(glyph("icons/plus.svg", 16., if selected { theme::text() } else { theme::text_soft() }))
            .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child("Invite someone"));
        column = column.child(
            list_row("tiny-invite-row", theme::row_height(theme::ROW_ONE), selected, false, content)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_invite(cx))),
        );
    }
    if !ready.members.is_empty() {
        column = column.child(section_header("People", !ready.can_invite));
        for (ix, member) in ready.members.iter().enumerate() {
            let selected = shown == Some(member.user_id.as_str());
            let key = Key { network: Network::Tiny, account: member.user_id.clone(), id: member.user_id.clone() };
            let content = div()
                .gap(px(10.))
                .child(Avatar::new(&key, &member.name, theme::row_avatar()).build())
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(member.name.clone()))
                        .child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(member.role.clone())),
                );
            let user_id = member.user_id.clone();
            column = column.child(
                list_row(("tiny-person", ix), theme::row_height(theme::ROW_TWO), selected, false, content)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.pick_person(user_id.clone(), cx))),
            );
        }
    }
    column.into_any_element()
}

fn server_view(ready: &Ready, cx: &mut Context<Shell>) -> AnyElement {
    if !ready.can_invite {
        return note_line("Nobody is on this server.", false).into_any_element();
    }
    let head = div()
        .px(px(theme::PANE_PAD))
        .pt(px(20.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().type_title().text_color(theme::text()).child("Invite someone"))
        .when(ready.token.is_none(), |column| {
            column.child(
                div()
                    .max_w(px(360.))
                    .whitespace_normal()
                    .type_ui()
                    .text_color(theme::text_soft())
                    .child("They paste the code into their own tinyclient."),
            )
        })
        .children(ready.error.as_ref().map(|detail| div().type_small().text_color(theme::danger()).child(detail.clone())))
        .children(ready.token.as_ref().map(|token| code_panel(token, cx)))
        .when(ready.token.is_none(), |column| column.child(invite_row(&ready.invite_role, ready.busy, cx)));
    let mut column = div().flex().flex_col().child(head);
    if !ready.invites.is_empty() {
        column = column.child(invite_section(&ready.invites));
    }
    if let Some(rows) = &ready.audit {
        if !rows.is_empty() {
            column = column.child(audit_section(rows));
        }
    }
    column.into_any_element()
}

fn person_view(member: &Member, mine: bool, confirm: Option<&str>, cx: &mut Context<Shell>) -> AnyElement {
    let confirming = confirm == Some(member.user_id.as_str());
    let actions = member.make_admin || member.make_member || member.make_owner || member.remove;
    let key = Key { network: Network::Tiny, account: member.user_id.clone(), id: member.user_id.clone() };
    div()
        .px(px(theme::PANE_PAD))
        .pt(px(24.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .child(Avatar::new(&key, &member.name, 64.).build())
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(member.name.clone()))
                        .child(div().type_ui().text_color(theme::text_soft()).child(member.role.clone()))
                        .when(!member.email.is_empty(), |column| {
                            column.child(div().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(member.email.clone()))
                        }),
                ),
        )
        .when(mine && member.role == "Owner" && !actions, |column| {
            column.child(div().max_w(px(420.)).whitespace_normal().type_ui().text_color(theme::text_soft()).child("You're the owner of this server."))
        })
        .when(!confirming && actions, |column| column.child(member_actions(member, cx)))
        .when(confirming, |column| column.child(confirm_remove(&member.name, &member.user_id, cx)))
        .into_any_element()
}

fn invite_row(role: &str, busy: bool, cx: &mut Context<Shell>) -> Div {
    let invite = if busy {
        button_disabled("Invite someone").into_any_element()
    } else {
        button("tiny-invite", ButtonKind::Primary, "Invite someone")
            .on_click(cx.listener(|this, _, _, cx| this.send_invite(cx)))
            .into_any_element()
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(
            segmented()
                .child(segment("tiny-as-member", "Member", role != "admin").on_click(cx.listener(|this, _, _, cx| this.pick_role("member", cx))))
                .child(segment("tiny-as-admin", "Admin", role == "admin").on_click(cx.listener(|this, _, _, cx| this.pick_role("admin", cx)))),
        )
        .child(invite)
}

fn code_panel(token: &str, cx: &mut Context<Shell>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child("Copy this code. It works for 7 days and will not be shown again."))
        .child(
            div()
                .px(px(12.))
                .py(px(8.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::field())
                .border_1()
                .border_color(theme::border())
                .whitespace_normal()
                .type_code()
                .child(token.to_owned()),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(button("tiny-copy", ButtonKind::Primary, "Copy code").on_click(cx.listener(|this, _, _, cx| this.copy_code(cx))))
                .child(button("tiny-done", ButtonKind::Secondary, "Done").on_click(cx.listener(|this, _, _, cx| this.done_code(cx)))),
        )
}

fn member_actions(member: &Member, cx: &mut Context<Shell>) -> Div {
    let mut actions = div().flex_none().flex().flex_wrap().items_center().gap(px(4.));
    if member.make_admin {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-admin-{}", member.user_id), ButtonKind::Secondary, "Make admin")
                .on_click(cx.listener(move |this, _, _, cx| this.send_role(user_id.clone(), "admin", cx))),
        );
    }
    if member.make_member {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-member-{}", member.user_id), ButtonKind::Secondary, "Make member")
                .on_click(cx.listener(move |this, _, _, cx| this.send_role(user_id.clone(), "member", cx))),
        );
    }
    if member.make_owner {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-owner-{}", member.user_id), ButtonKind::Secondary, "Make owner")
                .on_click(cx.listener(move |this, _, _, cx| this.send_role(user_id.clone(), "owner", cx))),
        );
    }
    if member.remove {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-remove-{}", member.user_id), ButtonKind::Danger, "Remove")
                .on_click(cx.listener(move |this, _, _, cx| this.ask_remove(user_id.clone(), cx))),
        );
    }
    actions
}

fn confirm_remove(name: &str, user_id: &str, cx: &mut Context<Shell>) -> Div {
    let remove_id = user_id.to_owned();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(div().whitespace_normal().type_ui().text_color(theme::text()).child(format!("Remove {name} from this server?")))
        .child(
            button(format!("tiny-remove-yes-{user_id}"), ButtonKind::Danger, "Remove")
                .on_click(cx.listener(move |this, _, _, cx| this.send_remove(remove_id.clone(), cx))),
        )
        .child(button(format!("tiny-remove-no-{user_id}"), ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_remove(cx))))
}

fn invite_section(invites: &[OpenInvite]) -> Div {
    div().mt(px(8.)).flex().flex_col().child(section_header("Invites", false)).children(invites.iter().map(|invite| {
        div()
            .px(px(theme::PANE_PAD))
            .h(px(theme::ROW_ONE))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().type_ui().text_color(theme::text()).child(invite.role.clone()))
            .child(div().type_small().text_color(theme::text_soft()).child(format!("Expires {}", short_day(&invite.expires))))
    }))
}

fn audit_section(rows: &[String]) -> Div {
    div().mt(px(8.)).flex().flex_col().child(section_header("Activity", false)).children(rows.iter().map(|row| {
        div()
            .px(px(theme::PANE_PAD))
            .min_h(px(theme::ROW_ONE))
            .flex()
            .items_center()
            .min_w_0()
            .whitespace_normal()
            .type_ui()
            .text_color(theme::text_soft())
            .child(row.clone())
    }))
}

/// A day from `YYYY-MM-DD…`. Anything else is left as the server sent it.
fn short_day(stamp: &str) -> String {
    let bytes = stamp.as_bytes();
    if bytes.len() >= 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        let year = &stamp[0..4];
        let month: usize = stamp[5..7].parse().unwrap_or(0);
        let day: usize = stamp[8..10].parse().unwrap_or(0);
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return format!("{} {day}, {year}", MONTHS[month - 1]);
        }
    }
    stamp.to_owned()
}

fn role_place(role: &str) -> &'static str {
    if same_role(role, "admin") {
        "an admin"
    } else if same_role(role, "member") {
        "a member"
    } else if same_role(role, "owner") {
        "an owner"
    } else {
        ""
    }
}

/// What one audit row says. Machine actions stay off the screen.
fn audit_line<'a>(action: &str, detail: &str, name_of: impl Fn(&str) -> Option<&'a str>) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(detail).unwrap_or(serde_json::Value::Null);
    let role = parsed.get("role").and_then(|value| value.as_str()).unwrap_or("");
    let user_id = parsed.get("user_id").and_then(|value| value.as_str()).unwrap_or("");
    let who = name_of(user_id).filter(|name| !name.is_empty());
    let place = role_place(role);
    Some(match action {
        "bootstrap" => "This server was created.".to_owned(),
        "invite.create" => match place {
            "an admin" => "An invite for an admin was created.".to_owned(),
            "a member" => "An invite for a member was created.".to_owned(),
            "an owner" => "An invite for an owner was created.".to_owned(),
            _ => "An invite was created.".to_owned(),
        },
        "invite.revoke" => "An invite was revoked.".to_owned(),
        "invite.accept" => match place {
            "an admin" => "An admin joined.".to_owned(),
            "a member" => "A member joined.".to_owned(),
            "an owner" => "An owner joined.".to_owned(),
            _ => "Someone joined.".to_owned(),
        },
        "member.role" => match (who, place) {
            (Some(name), place) if !place.is_empty() => format!("{name} is now {place}."),
            (None, place) if !place.is_empty() => format!("Someone is now {place}."),
            _ => "A role changed.".to_owned(),
        },
        "member.remove" => match who {
            Some(name) => format!("{name} was removed."),
            None => "Someone was removed.".to_owned(),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_buttons_follow_who_may_change_whom() {
        let owners = 1;
        assert!(role_change_legal("owner", "member", "admin", owners));
        assert!(role_change_legal("owner", "member", "owner", owners));
        assert!(role_change_legal("admin", "member", "admin", owners));
        assert!(!role_change_legal("admin", "member", "owner", owners));
        assert!(!role_change_legal("admin", "admin", "member", owners));
        assert!(!role_change_legal("admin", "owner", "admin", owners));
        assert!(!role_change_legal("admin", "owner", "member", owners));
        assert!(!role_change_legal("owner", "owner", "admin", owners));
        assert!(!role_change_legal("owner", "owner", "member", owners));
        assert!(!role_change_legal("member", "member", "admin", owners));
        assert!(role_change_legal("owner", "owner", "admin", 2));
        assert!(role_change_legal("owner", "admin", "member", owners));
        assert!(!role_change_legal("owner", "admin", "admin", owners));
    }

    #[test]
    fn remove_spares_the_only_owner_and_an_admin_does_not_touch_one() {
        assert!(!remove_legal("owner", "owner", 1));
        assert!(remove_legal("owner", "owner", 2));
        assert!(remove_legal("owner", "admin", 1));
        assert!(remove_legal("owner", "member", 1));
        assert!(remove_legal("admin", "member", 1));
        assert!(!remove_legal("admin", "admin", 1));
        assert!(!remove_legal("admin", "owner", 2));
        assert!(!remove_legal("member", "member", 1));
    }

    fn one(id: &str) -> Member {
        Member {
            user_id: id.into(),
            name: id.into(),
            email: String::new(),
            role: "Member".into(),
            make_admin: false,
            make_member: false,
            make_owner: false,
            remove: false,
        }
    }

    #[test]
    fn the_invite_is_on_show_until_a_person_is_picked_and_a_member_lands_on_someone() {
        let people = [one("ada"), one("bo")];
        assert_eq!(shown_id(None, true, &people), None);
        assert_eq!(shown_id(Some("bo"), true, &people), Some("bo"));
        assert_eq!(shown_id(Some("gone"), true, &people), None);
        assert_eq!(shown_id(None, false, &people), Some("ada"));
        assert_eq!(shown_id(Some("gone"), false, &people), Some("ada"));
    }

    #[test]
    fn an_audit_row_is_a_sentence_and_a_stamp_is_a_day() {
        assert_eq!(audit_line("bootstrap", "", |_| None).as_deref(), Some("This server was created."));
        assert_eq!(audit_line("invite.create", r#"{"role":"admin"}"#, |_| None).as_deref(), Some("An invite for an admin was created."));
        assert_eq!(audit_line("invite.create", r#"{"role":"member"}"#, |_| None).as_deref(), Some("An invite for a member was created."));
        assert_eq!(audit_line("invite.revoke", "{}", |_| None).as_deref(), Some("An invite was revoked."));
        assert_eq!(audit_line("invite.accept", r#"{"role":"member"}"#, |_| None).as_deref(), Some("A member joined."));
        assert_eq!(
            audit_line("member.role", r#"{"user_id":"a","role":"admin"}"#, |id| (id == "a").then_some("Ada")).as_deref(),
            Some("Ada is now an admin.")
        );
        assert_eq!(
            audit_line("member.role", r#"{"user_id":"a","role":"member"}"#, |id| (id == "a").then_some("Ada")).as_deref(),
            Some("Ada is now a member.")
        );
        assert_eq!(audit_line("member.remove", r#"{"user_id":"gone"}"#, |_| None).as_deref(), Some("Someone was removed."));
        assert_eq!(audit_line("member.remove", r#"{"user_id":"a"}"#, |id| (id == "a").then_some("Ada")).as_deref(), Some("Ada was removed."));
        assert_eq!(audit_line("nope", "membership", |_| None), None);
        assert_eq!(short_day("2026-11-01T00:00:00Z"), "Nov 1, 2026");
        assert_eq!(short_day("soon"), "soon");
    }
}
