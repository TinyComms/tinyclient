//! Audit log: what was done on the server, newest first, each as a sentence with names ("Mara
//! Okafor gave Leila Haddad new roles."), a glyph for its kind, how long ago and the exact time,
//! and the reason when one was given. Filter by who did it and by kind; older entries come a
//! page at a time (`TinyAudit`). The server setup's rows (one per owner invite it printed) read as
//! one line.
//!
//! On a server older than audit queries, the entries the page was read with are all there is:
//! no filters, no paging.

use super::kit::{self, card};
use super::page_head;
use crate::client::ask;
use crate::gui::components::{button, button_disabled, glyph, menu, menu_item_check, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::Page as Facts;
use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, Context, SharedString, Window};
use protocol::{Request, Response, TinyAudit, TinyAuditFilter, TinyCommunity, TINY_FEATURE_AUDIT_QUERY};
use serde_json::Value;

/// Rows read per page.
pub(in crate::gui) const PAGE: u32 = 50;

/// The kinds the filter offers, with the action prefix each asks for.
pub(in crate::gui) const KINDS: [(&str, &str); 8] =
    [("Everything", ""), ("Members", "member."), ("Roles", "role."), ("Invites", "invite."), ("Channels", "room."), ("Categories", "category."), ("Voice moderation", "call."), ("Server", "server.")];

/// Which filter list is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Pick {
    Person,
    Kind,
}

/// The entries read so far for the filters chosen, and where the reading is.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub rows: Vec<TinyAudit>,
    pub loading: bool,
    /// The first page has been asked for.
    pub asked: bool,
    /// The last page read was short: there is nothing older.
    pub end: bool,
    pub error: Option<String>,
    pub actor: Option<String>,
    pub kind: &'static str,
    pub open: Option<Pick>,
    /// Bumped when the filters change, so an answer for old filters is dropped.
    pub epoch: u64,
}

/// Rows with the server setup's run of rows read as one: each row and how many it stands for.
pub(in crate::gui) fn collapse(rows: &[TinyAudit]) -> Vec<(&TinyAudit, usize)> {
    let mut out: Vec<(&TinyAudit, usize)> = Vec::new();
    for row in rows {
        match out.last_mut() {
            Some((last, count)) if last.action == "bootstrap" && row.action == "bootstrap" => *count += 1,
            _ => out.push((row, 1)),
        }
    }
    out
}

/// Who did it: the name the server sent, else theirs on the page, else the server itself.
pub(in crate::gui) fn actor(community: &TinyCommunity, row: &TinyAudit) -> String {
    if !row.actor_name.trim().is_empty() {
        return row.actor_name.clone();
    }
    match row.actor_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => model::name_by_id(community, id),
        None => "Server setup".to_owned(),
    }
}

/// To whom: the name the server sent, else theirs on the page, else from the detail.
fn subject(community: &TinyCommunity, row: &TinyAudit, detail: &Value) -> String {
    if !row.subject_name.trim().is_empty() {
        return row.subject_name.clone();
    }
    let id = row.subject_user_id.clone().or_else(|| detail.get("user_id").and_then(Value::as_str).map(str::to_owned));
    match id {
        Some(id) if community.member(&id).is_some() => model::name_by_id(community, &id),
        _ => "someone".to_owned(),
    }
}

fn place(role: &str) -> &'static str {
    match role.to_ascii_lowercase().as_str() {
        "owner" => "an owner",
        "admin" => "an admin",
        _ => "a member",
    }
}

/// What one row says, as a sentence. `count` is how many setup rows it stands for.
pub(in crate::gui) fn sentence(community: &TinyCommunity, row: &TinyAudit, count: usize) -> String {
    let detail: Value = serde_json::from_str(&row.detail).unwrap_or(Value::Null);
    let text = |key: &str| detail.get(key).and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_owned);
    let flag = |key: &str| detail.get(key).and_then(Value::as_bool);
    let who = actor(community, row);
    let whom = subject(community, row, &detail);
    let named = |what: &str| text("name").map_or_else(|| format!("a {what}"), |name| format!("the {what} {name}"));
    let room_name =
        || text("room_name").or_else(|| text("room_id").and_then(|id| community.rooms.iter().find(|room| room.id == id).map(|room| room.name.clone()))).unwrap_or_else(|| "another channel".to_owned());
    match row.action.as_str() {
        "bootstrap" if count > 1 => {
            format!("The server was set up, and {count} owner invites were printed.")
        }
        "bootstrap" => "The server was set up, and an owner invite was printed.".to_owned(),
        "server.update" => format!("{who} changed the server's settings."),
        "server.icon" => format!("{who} changed the server's icon."),
        "account.avatar" => format!("{who} changed their picture."),
        "role.create" => format!("{who} created {}.", named("role")),
        "role.update" => format!("{who} changed {}.", named("role")),
        "role.delete" => format!("{who} deleted {}.", named("role")),
        "role.order" => format!("{who} reordered the roles."),
        "member.roles" => format!("{who} changed {whom}'s roles."),
        "member.role" => match text("role") {
            Some(role) => format!("{who} made {whom} {}.", place(&role)),
            None => format!("{who} changed {whom}'s role."),
        },
        "member.owner" => match flag("owner") {
            Some(false) => format!("{who} took ownership from {whom}."),
            _ => format!("{who} made {whom} an owner."),
        },
        "member.timeout" => match detail.get("until") {
            Some(Value::Null) => format!("{who} ended {whom}'s timeout."),
            _ => format!("{who} timed out {whom}."),
        },
        "member.ban" => format!("{who} banned {whom}."),
        "member.unban" => format!("{who} unbanned {whom}."),
        "member.remove" | "member.kick" => format!("{who} kicked {whom}."),
        "invite.create" => match text("label") {
            Some(label) => format!("{who} created an invite: “{label}”."),
            None => format!("{who} created an invite."),
        },
        "invite.revoke" => format!("{who} revoked an invite."),
        "invite.accept" => {
            let joined = if row.subject_name.is_empty() && row.subject_user_id.is_none() { who } else { whom };
            format!("{joined} joined with an invite.")
        }
        "category.create" => format!("{who} created {}.", named("category")),
        "category.update" | "category.rename" => format!("{who} renamed {}.", named("category")),
        "category.delete" => format!("{who} deleted {}.", named("category")),
        "layout.update" => format!("{who} rearranged the channels."),
        "room.create" => format!("{who} created {}.", named("channel")),
        "room.update" | "room.rename" => format!("{who} changed {}.", named("channel")),
        "room.delete" => format!("{who} deleted {}.", named("channel")),
        "room.override" => format!("{who} changed who may do what in {}.", named("channel")),
        "call.server_mute" => match flag("server_muted") {
            Some(false) => format!("{who} let {whom} speak again."),
            _ => format!("{who} server-muted {whom}."),
        },
        "call.server_deafen" => match flag("server_deafened") {
            Some(false) => format!("{who} let {whom} hear again."),
            _ => format!("{who} server-deafened {whom}."),
        },
        "call.move" => format!("{who} moved {whom} to {}.", room_name()),
        "call.kick" | "call.disconnect" => format!("{who} disconnected {whom} from a call."),
        other => format!("{who} did {}.", other.replace(['.', '_'], " ")),
    }
}

/// The glyph for a row's kind, and its tone.
pub(in crate::gui) fn icon(action: &str) -> (&'static str, gpui::Rgba) {
    match action {
        "bootstrap" => ("icons/badge-check.svg", theme::accent_text()),
        "member.ban" | "member.unban" => ("icons/ban.svg", theme::danger()),
        "member.remove" | "member.kick" => ("icons/user-x.svg", theme::danger()),
        "member.timeout" => ("icons/timer.svg", theme::warning()),
        "member.owner" => ("icons/crown.svg", theme::warning()),
        "call.move" => ("icons/arrow-right-left.svg", theme::text_soft()),
        "call.server_deafen" => ("icons/headphone-off.svg", theme::warning()),
        _ if action.starts_with("call.") => ("icons/mic-off.svg", theme::warning()),
        _ if action.starts_with("member.") => ("icons/users.svg", theme::text_soft()),
        _ if action.starts_with("role.") => ("icons/shield.svg", theme::accent_text()),
        _ if action.starts_with("invite.") => ("icons/user-plus.svg", theme::positive()),
        _ if action.starts_with("room.") || action.starts_with("category.") || action.starts_with("layout.") => ("icons/volume-2.svg", theme::text_soft()),
        _ if action.starts_with("server.") => ("icons/server.svg", theme::text_soft()),
        _ => ("icons/list.svg", theme::text_faint()),
    }
}

/// How long ago, in the words a log uses: as `model::relative` for the last two months, then in
/// months and years.
pub(in crate::gui) fn ago(now: i64, then: i64) -> String {
    let days = (now - then) / 86_400;
    match days {
        ..60 => model::relative(now, then),
        60..365 => format!("{} months ago", days / 30),
        365..730 => "a year ago".to_owned(),
        _ => format!("{} years ago", days / 365),
    }
}

/// The filter the next page is read with.
pub(in crate::gui) fn filter(draft: &Draft, more: bool) -> TinyAuditFilter {
    TinyAuditFilter { limit: PAGE, before: if more { draft.rows.last().map(|row| row.id) } else { None }, actor: draft.actor.clone(), action: draft.kind.to_owned() }
}

impl Shell {
    /// Reads the first page for the filters chosen, or with `more` the page after the last row.
    pub(in crate::gui) fn tiny_audit_load(&mut self, more: bool, cx: &mut Context<Self>) {
        let (Some(account), Some(community)) = (self.tiny_state.account.clone(), self.tiny_state.community.as_ref()) else {
            return;
        };
        let queries = community.has(TINY_FEATURE_AUDIT_QUERY);
        let kept = community.audit.clone();
        let Some(overlay) = self.tiny_state.settings.as_mut() else {
            return;
        };
        let draft = &mut overlay.audit;
        draft.asked = true;
        if !queries {
            // An older server: what the page was read with is the whole of it.
            draft.rows = kept;
            draft.end = true;
            cx.notify();
            return;
        }
        if more && (draft.loading || draft.end) {
            return;
        }
        if !more {
            draft.rows.clear();
            draft.end = false;
            draft.epoch += 1;
        }
        draft.loading = true;
        draft.error = None;
        let (epoch, filter) = (draft.epoch, filter(draft, more));
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyAudit { account, filter }),
            move |this, answer, cx| {
                let Some(draft) = this.tiny_state.settings.as_mut().map(|overlay| &mut overlay.audit).filter(|draft| draft.epoch == epoch) else {
                    return;
                };
                draft.loading = false;
                match answer {
                    Ok(Response::TinyAudit { audit }) => {
                        draft.end = (audit.len() as u32) < PAGE;
                        let known: std::collections::HashSet<i64> = draft.rows.iter().map(|row| row.id).collect();
                        draft.rows.extend(audit.into_iter().filter(|row| !known.contains(&row.id)));
                    }
                    Ok(Response::Error { detail, .. }) => draft.error = Some(detail),
                    Ok(_) => draft.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => draft.error = Some(crate::client::trouble_words(&err)),
                }
                cx.notify();
            },
        );
    }

    fn tiny_audit_filter(&mut self, change: impl FnOnce(&mut Draft), cx: &mut Context<Self>) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            change(&mut overlay.audit);
            overlay.audit.open = None;
        }
        self.tiny_audit_load(false, cx);
    }

    fn tiny_audit_open(&mut self, pick: Option<Pick>, cx: &mut Context<Self>) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.audit.open = pick;
        }
        cx.notify();
    }
}

fn dropdown<E: InteractiveElement + Styled + IntoElement + 'static>(list: E, cx: &mut Context<Shell>) -> AnyElement {
    let list = list.max_h(px(360.)).on_mouse_down_out(cx.listener(|this, _, _, cx| this.tiny_audit_open(None, cx)));
    deferred(anchored().snap_to_window().child(div().mt(px(4.)).child(list))).with_priority(2).into_any_element()
}

pub(in crate::gui::tiny) fn audit(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = window;
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.audit;
    let queries = community.has(TINY_FEATURE_AUDIT_QUERY);
    let mut page = div().flex().flex_col().gap(px(20.)).child(page_head("Audit log", "What was done on this server, by whom, and why."));
    if queries {
        let person = draft.actor.as_ref().map_or("Anyone".to_owned(), |id| model::name_by_id(community, id));
        let kind = KINDS.iter().find(|(_, prefix)| *prefix == draft.kind).map_or("Everything", |(words, _)| *words);
        let open = draft.open;
        let mut people = div().flex().flex_col().child(
            kit::picker("tiny-audit-person", "icons/user.svg", person, open == Some(Pick::Person), 200.)
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_audit_open(if open == Some(Pick::Person) { None } else { Some(Pick::Person) }, cx))),
        );
        if open == Some(Pick::Person) {
            let mut list = menu()
                .id("tiny-audit-people")
                .overflow_y_scroll()
                .w(px(240.))
                .child(menu_item_check("tiny-audit-anyone", draft.actor.is_none(), "Anyone", None).on_click(cx.listener(|this, _, _, cx| this.tiny_audit_filter(|draft| draft.actor = None, cx))));
            let mut members: Vec<_> = community.members.iter().collect();
            members.sort_by_key(|member| model::name_of(member).to_lowercase());
            for (ix, member) in members.into_iter().enumerate() {
                let id = member.user_id.clone();
                list = list.child(menu_item_check(("tiny-audit-actor", ix), draft.actor.as_deref() == Some(member.user_id.as_str()), model::name_of(member), None).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let id = id.clone();
                        this.tiny_audit_filter(|draft| draft.actor = Some(id), cx)
                    },
                )));
            }
            people = people.child(dropdown(list, cx));
        }
        let mut kinds = div().flex().flex_col().child(
            kit::picker("tiny-audit-kind", "icons/list.svg", kind, open == Some(Pick::Kind), 200.)
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_audit_open(if open == Some(Pick::Kind) { None } else { Some(Pick::Kind) }, cx))),
        );
        if open == Some(Pick::Kind) {
            let mut list = menu().w(px(220.));
            for (ix, (words, prefix)) in KINDS.iter().enumerate() {
                let prefix: &'static str = prefix;
                list = list.child(
                    menu_item_check(("tiny-audit-kinds", ix), draft.kind == prefix, *words, None).on_click(cx.listener(move |this, _, _, cx| this.tiny_audit_filter(|draft| draft.kind = prefix, cx))),
                );
            }
            kinds = kinds.child(dropdown(list, cx));
        }
        page = page.child(div().flex().items_center().gap(px(8.)).child(people).child(kinds));
    }
    if let Some(error) = &draft.error {
        page = page.child(kit::note("icons/circle-alert.svg", theme::danger(), error.clone()));
    }
    let rows = collapse(&draft.rows);
    if rows.is_empty() {
        let words = if draft.loading || !draft.asked {
            "Reading the log…"
        } else if draft.actor.is_some() || !draft.kind.is_empty() {
            "Nothing matches these filters."
        } else {
            "Nothing has been done yet."
        };
        return page.child(kit::empty("icons/list.svg", words)).into_any_element();
    }
    let now = kit::now();
    let mut list = card();
    let last = rows.len();
    for (ix, (row, count)) in rows.into_iter().enumerate() {
        let (glyph_name, tone) = icon(&row.action);
        let at = protocol::tiny_unix(&row.created_at);
        let when = at.map(|at| ago(now, at)).unwrap_or_default();
        let exact = at.map(|at| format!("{} at {}", crate::gui::time::date(at), crate::gui::time::clock(at))).unwrap_or_else(|| row.created_at.clone());
        list = list.child(
            div()
                .id(SharedString::from(format!("tiny-audit-row-{}", row.id)))
                .px(px(14.))
                .py(px(12.))
                .flex()
                .items_start()
                .gap(px(12.))
                .when(ix + 1 < last, |row| row.border_b_1().border_color(theme::hairline()))
                .child(div().flex_none().size(px(32.)).rounded_full().bg(theme::hover()).flex().items_center().justify_center().child(glyph(glyph_name, 16., tone)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().whitespace_normal().type_ui().text_color(theme::text()).child(sentence(community, row, count)))
                        .when(!row.reason.trim().is_empty(), |column| {
                            column.child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(6.))
                                    .child(div().type_small().text_color(theme::text_faint()).child("Reason:"))
                                    .child(div().flex_1().whitespace_normal().type_small().text_color(theme::text_soft()).child(row.reason.clone())),
                            )
                        })
                        .child(div().type_small().text_color(theme::text_faint()).child(format!("{when} · {exact}"))),
                ),
        );
    }
    page = page.child(list);
    if draft.loading {
        page = page.child(div().flex().justify_center().child(button_disabled("Reading…")));
    } else if !draft.end {
        page = page
            .child(div().flex().justify_center().child(button("tiny-audit-more", ButtonKind::Secondary, "Load older entries").on_click(cx.listener(|this, _, _, cx| this.tiny_audit_load(true, cx)))));
    } else if queries {
        page = page.child(div().flex().justify_center().type_small().text_color(theme::text_faint()).child("That's everything."));
    }
    page.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, actor: Option<&str>, action: &str) -> TinyAudit {
        TinyAudit { id, actor_id: actor.map(str::to_owned), action: action.into(), ..TinyAudit::default() }
    }

    fn community() -> TinyCommunity {
        serde_json::from_value(serde_json::json!({"role":"owner","members":[{"user_id":"m","display_name":"Mara"},{"user_id":"l","display_name":"Leila"}],"invites":[],"audit":[]})).unwrap()
    }

    #[test]
    fn the_setup_rows_read_as_one_and_nothing_else_collapses() {
        let rows = [row(9, Some("m"), "role.create"), row(3, None, "bootstrap"), row(2, None, "bootstrap"), row(1, None, "bootstrap"), row(0, Some("m"), "role.create")];
        let collapsed: Vec<_> = collapse(&rows).into_iter().map(|(row, count)| (row.id, count)).collect();
        assert_eq!(collapsed, [(9, 1), (3, 3), (0, 1)]);
        let community = community();
        assert_eq!(sentence(&community, &rows[1], 3), "The server was set up, and 3 owner invites were printed.");
    }

    #[test]
    fn a_row_is_a_sentence_with_names() {
        let community = community();
        let with = |action: &str, detail: &str| TinyAudit { actor_id: Some("m".into()), subject_user_id: Some("l".into()), action: action.into(), detail: detail.into(), ..TinyAudit::default() };
        assert_eq!(sentence(&community, &with("member.roles", ""), 1), "Mara changed Leila's roles.");
        assert_eq!(sentence(&community, &with("member.owner", r#"{"owner":false}"#), 1), "Mara took ownership from Leila.");
        assert_eq!(sentence(&community, &with("member.timeout", r#"{"until":null}"#), 1), "Mara ended Leila's timeout.");
        assert_eq!(sentence(&community, &with("role.create", r#"{"name":"Raid crew"}"#), 1), "Mara created the role Raid crew.");
        assert_eq!(sentence(&community, &with("room.delete", ""), 1), "Mara deleted a channel.");
        assert_eq!(sentence(&community, &with("member.role", r#"{"role":"admin"}"#), 1), "Mara made Leila an admin.");
        let named = TinyAudit { actor_name: "Robin Vale".into(), subject_name: "Pavel Orlov".into(), ..with("member.ban", "") };
        assert_eq!(sentence(&community, &named, 1), "Robin Vale banned Pavel Orlov.");
        let system = TinyAudit { actor_id: None, ..with("invite.create", r#"{"label":"Owner"}"#) };
        assert_eq!(sentence(&community, &system, 1), "Server setup created an invite: “Owner”.");
        assert_eq!(sentence(&community, &with("weird.thing", ""), 1), "Mara did weird thing.");
        assert_eq!(icon("member.ban").0, "icons/ban.svg");
        assert_eq!(icon("role.order").0, "icons/shield.svg");
    }

    #[test]
    fn old_entries_are_months_and_years_ago() {
        let day = 86_400;
        assert_eq!(ago(100 * day, 99 * day), "1 day ago");
        assert_eq!(ago(400 * day, 300 * day), "3 months ago");
        assert_eq!(ago(800 * day, 300 * day), "a year ago");
        assert_eq!(ago(1200 * day, 300 * day), "2 years ago");
    }

    #[test]
    fn the_next_page_starts_after_the_last_row_with_the_filters_chosen() {
        let draft = Draft { rows: vec![row(40, None, "x"), row(31, None, "x")], actor: Some("m".into()), kind: "member.", ..Draft::default() };
        let next = filter(&draft, true);
        assert_eq!((next.before, next.actor.as_deref(), next.action.as_str(), next.limit), (Some(31), Some("m"), "member.", PAGE));
        assert_eq!(filter(&draft, false).before, None);
    }
}
