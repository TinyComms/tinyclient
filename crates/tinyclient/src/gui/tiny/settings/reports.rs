//! Abuse reports: moderator queue (`GET /v1/reports`), full report view (`GET /v1/reports/{id}`),
//! and handling actions (`PATCH /v1/reports/{id}`: status, moderator note, link audit action,
//! owner legal hold).
//!
//! Report text, reporter notes, evidence, and moderator notes live in memory here only and are
//! dropped when Server Settings closes. Nothing is cached to disk or logged. Evidence is always
//! labelled "Sent by the reporter. Not verified."

use super::kit::{self, card};
use super::page_head;
use crate::client::ask;
use crate::gui::components::{button, button_disabled, field, glyph, segment, segmented, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::{Float, Page as Facts};
use gpui::{div, prelude::*, px, AnyElement, Context, Focusable, MouseButton, MouseDownEvent, Window};
use protocol::{Request, Response, TinyAudit, TinyReportDetail, TinyReportFilter, TinyReportSummary, TinyReportUpdate};

const PAGE: u32 = 50;

const STATUSES: [(&str, &str); 5] = [
    ("Open", "open"),
    ("Reviewing", "reviewing"),
    ("Actioned", "actioned"),
    ("Dismissed", "dismissed"),
    ("All", ""),
];

const KINDS: [(&str, &str); 4] = [
    ("All kinds", ""),
    ("Message", "message"),
    ("Member", "member"),
    ("Channel", "channel"),
];

const ACTIONS: [(&str, &str); 4] = [
    ("Open", "open"),
    ("Reviewing", "reviewing"),
    ("Actioned", "actioned"),
    ("Dismissed", "dismissed"),
];

/// In-memory state for the Reports page. Dropped when Server Settings closes.
pub(in crate::gui) struct Draft {
    pub rows: Vec<TinyReportSummary>,
    pub selected: Option<TinyReportDetail>,
    pub status: &'static str,
    pub kind: &'static str,
    pub loading: bool,
    pub asked: bool,
    pub end: bool,
    pub reading: bool,
    pub working: bool,
    pub error: Option<String>,
    pub detail_error: Option<String>,
    pub epoch: u64,
}

impl Default for Draft {
    fn default() -> Draft {
        Draft {
            rows: Vec::new(),
            selected: None,
            status: "open",
            kind: "",
            loading: false,
            asked: false,
            end: false,
            reading: false,
            working: false,
            error: None,
            detail_error: None,
            epoch: 0,
        }
    }
}

fn person_label(person: Option<&protocol::TinyReportPerson>) -> String {
    match person {
        Some(p) if !p.display_name.trim().is_empty() => p.display_name.clone(),
        Some(p) if !p.user_id.is_empty() => p.user_id.clone(),
        _ => "Unknown".to_owned(),
    }
}

/// Recent kicks, bans, and timeouts against the reported user that can be linked to this report.
fn linkable_actions<'a>(audit: &'a [TinyAudit], reported_user_id: Option<&str>, linked: &[i64]) -> Vec<&'a TinyAudit> {
    audit
        .iter()
        .filter(|row| matches!(row.action.as_str(), "member.remove" | "member.ban" | "member.timeout"))
        .filter(|row| !linked.contains(&row.id))
        .filter(|row| match reported_user_id {
            Some(uid) if !uid.is_empty() => row.subject_user_id.as_deref() == Some(uid),
            _ => true,
        })
        .take(5)
        .collect()
}

pub(in crate::gui::tiny) fn reports(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.reports;

    let mut status_seg = segmented();
    for (ix, (label, val)) in STATUSES.iter().enumerate() {
        let val = *val;
        status_seg = status_seg.child(segment(("tiny-rep-status", ix), *label, draft.status == val).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(overlay) = this.tiny_state.settings.as_mut() {
                overlay.reports.status = val;
                overlay.reports.selected = None;
            }
            this.tiny_reports_load(false, cx);
        })));
    }

    let mut kind_seg = segmented();
    for (ix, (label, val)) in KINDS.iter().enumerate() {
        let val = *val;
        kind_seg = kind_seg.child(segment(("tiny-rep-kind", ix), *label, draft.kind == val).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(overlay) = this.tiny_state.settings.as_mut() {
                overlay.reports.kind = val;
                overlay.reports.selected = None;
            }
            this.tiny_reports_load(false, cx);
        })));
    }

    let refresh = if draft.loading {
        button_disabled("Refreshing…").h(px(30.)).into_any_element()
    } else {
        button("tiny-rep-refresh", ButtonKind::Secondary, "Refresh")
            .h(px(30.))
            .on_click(cx.listener(|this, _, _, cx| this.tiny_reports_load(false, cx)))
            .into_any_element()
    };

    let controls = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(status_seg)
        .child(kind_seg)
        .child(div().flex_1())
        .child(refresh);

    let mut page = div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(page_head("Reports", "Member reports for moderators. Resolved reports are removed after the server's retention period unless held."))
        .child(controls);

    if let Some(detail) = &draft.error {
        page = page.child(div().type_small().text_color(theme::danger()).child(detail.clone()));
    }

    if let Some(detail) = &draft.selected {
        page = page.child(detail_card(shell, facts, detail, draft.working, draft.detail_error.as_deref(), window, cx));
    } else if draft.reading {
        page = page.child(card().p(px(14.)).child(div().type_small().text_color(theme::text_faint()).child("Loading report…")));
    }

    let mut list = card();
    if draft.rows.is_empty() {
        let msg = if draft.loading { "Loading reports…" } else { "No reports match these filters." };
        list = list.p(px(14.)).child(div().type_small().text_color(theme::text_faint()).child(msg));
    } else {
        for (ix, row) in draft.rows.iter().enumerate() {
            let selected = draft.selected.as_ref().is_some_and(|d| d.summary.id == row.id);
            let id = row.id.clone();
            let reported = person_label(row.reported.as_ref());
            let reporter = person_label(row.reporter.as_ref());
            let mut item = div()
                .id(("tiny-rep-row", ix))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(theme::R_CONTROL))
                .cursor_pointer()
                .when(selected, |r| r.bg(theme::selected()))
                .when(!selected, |r| r.hover(|s| s.bg(theme::hover())))
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_report_open(id.clone(), cx)))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(format!("{} · {}", row.category, row.kind)))
                                .child(div().type_micro().text_color(theme::text_faint()).child(row.status.clone())),
                        )
                        .child(
                            div()
                                .type_small()
                                .text_color(theme::text_soft())
                                .truncate()
                                .child(format!("Reported: {reported} · By: {reporter} · {}", row.created_at)),
                        ),
                );
            if row.has_evidence {
                item = item.child(glyph("icons/paperclip.svg", 14., theme::text_faint()));
            }
            if row.preserve {
                item = item.child(glyph("icons/lock.svg", 14., theme::warning()));
            }
            list = list.child(item);
        }
    }
    page = page.child(list);

    if !draft.rows.is_empty() && !draft.end {
        let more = if draft.loading {
            button_disabled("Loading…").h(px(30.)).into_any_element()
        } else {
            button("tiny-rep-more", ButtonKind::Secondary, "Load more")
                .h(px(30.))
                .on_click(cx.listener(|this, _, _, cx| this.tiny_reports_load(true, cx)))
                .into_any_element()
        };
        page = page.child(div().flex().justify_center().child(more));
    }

    page.into_any_element()
}

fn detail_card(
    shell: &Shell,
    facts: &Facts,
    detail: &TinyReportDetail,
    working: bool,
    error: Option<&str>,
    window: &Window,
    cx: &mut Context<Shell>,
) -> gpui::Div {
    let summary = &detail.summary;
    let report_id = summary.id.clone();
    let reported_name = person_label(summary.reported.as_ref());
    let reporter_name = person_label(summary.reporter.as_ref());
    let reported_uid = summary.reported.as_ref().map(|p| p.user_id.clone()).filter(|id| !id.is_empty());

    let mut header = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(div().type_title().text_color(theme::text()).child(format!("{} report ({})", summary.category, summary.kind)))
                .child(div().type_small().text_color(theme::text_soft()).child(format!("Reported: {reported_name} · Reporter: {reporter_name} · {}", summary.created_at))),
        );

    if let Some(uid) = reported_uid.clone() {
        let menu_uid = uid.clone();
        header = header.child(
            button("tiny-rep-person-menu", ButtonKind::Secondary, "Member actions")
                .h(px(28.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.tiny_open_float(Float::Person { user_id: menu_uid.clone(), at: event.position }, cx);
                    }),
                ),
        );
    }

    header = header.child(
        button("tiny-rep-close", ButtonKind::Ghost, "Close")
            .h(px(28.))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(overlay) = this.tiny_state.settings.as_mut() {
                    overlay.reports.selected = None;
                    overlay.reports.detail_error = None;
                }
                cx.notify();
            })),
    );

    let mut body = card().p(px(16.)).gap(px(14.)).child(header);

    if let Some(err) = error {
        body = body.child(div().type_small().text_color(theme::danger()).child(err.to_owned()));
    }

    if let Some(note) = detail.note.as_deref().filter(|n| !n.trim().is_empty()) {
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().type_micro().text_color(theme::text_faint()).child("REPORTER'S NOTE"))
                .child(div().whitespace_normal().type_ui().text_color(theme::text()).child(note.to_owned())),
        );
    }

    if let Some(evidence) = detail.evidence.as_deref().filter(|e| !e.trim().is_empty()) {
        body = body.child(
            div()
                .p(px(12.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::canvas())
                .border_1()
                .border_color(theme::warning())
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().type_micro().font_weight(theme::SEMIBOLD).text_color(theme::warning()).child("Sent by the reporter. Not verified."))
                .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(evidence.to_owned())),
        );
    }

    // Status buttons
    let mut status_row = div().flex().flex_wrap().items_center().gap(px(6.));
    for (ix, (label, val)) in ACTIONS.iter().enumerate() {
        let active = summary.status == *val;
        let id = report_id.clone();
        let status_val = (*val).to_owned();
        let kind = if active { ButtonKind::Primary } else { ButtonKind::Secondary };
        let btn = if working {
            button_disabled(*label).h(px(28.)).into_any_element()
        } else {
            button(("tiny-rep-act", ix), kind, *label)
                .h(px(28.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_report_work(id.clone(), TinyReportUpdate { status: Some(status_val.clone()), ..TinyReportUpdate::default() }, cx);
                }))
                .into_any_element()
        };
        status_row = status_row.child(btn);
    }
    if facts.may.owner {
        let id = report_id.clone();
        let next = !summary.preserve;
        let label = if summary.preserve { "Release legal hold" } else { "Hold for legal reasons" };
        let hold = if working {
            button_disabled(label).h(px(28.)).into_any_element()
        } else {
            button("tiny-rep-preserve", ButtonKind::Secondary, label)
                .h(px(28.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_report_work(id.clone(), TinyReportUpdate { preserve: Some(next), ..TinyReportUpdate::default() }, cx);
                }))
                .into_any_element()
        };
        status_row = status_row.child(div().flex_1()).child(hold);
    }
    body = body.child(status_row);

    // Link an action from recent kicks/bans/timeouts on this member
    let actions = linkable_actions(&facts.community.audit, reported_uid.as_deref(), &summary.linked_audit_ids);
    if !actions.is_empty() {
        let mut links = div().flex().flex_wrap().items_center().gap(px(6.)).child(div().type_micro().text_color(theme::text_faint()).child("LINK AN ACTION"));
        for (ix, audit_row) in actions.into_iter().enumerate() {
            let id = report_id.clone();
            let audit_id = audit_row.id;
            let label = format!("{} (#{})", audit_row.action, audit_row.id);
            links = links.child(
                button(("tiny-rep-link", ix), ButtonKind::Ghost, label)
                    .h(px(26.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tiny_report_work(id.clone(), TinyReportUpdate { link_audit_id: Some(audit_id), ..TinyReportUpdate::default() }, cx);
                    })),
            );
        }
        body = body.child(links);
    }

    // Moderator notes + add note field
    let mut notes_box = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().type_micro().text_color(theme::text_faint()).child("MODERATOR NOTES"));
    if detail.notes.is_empty() {
        notes_box = notes_box.child(div().type_small().text_color(theme::text_faint()).child("No moderator notes yet."));
    } else {
        for note in &detail.notes {
            let who = person_label(note.author.as_ref());
            notes_box = notes_box.child(
                div()
                    .p(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().type_micro().text_color(theme::text_faint()).child(format!("{who} · {}", note.created_at)))
                    .child(div().whitespace_normal().type_small().text_color(theme::text()).child(note.body.clone())),
            );
        }
    }
    let note_input = &shell.tiny_state.settings_fields.report_note;
    let focused = note_input.focus_handle(cx).is_focused(window);
    let add_id = report_id.clone();
    let add_btn = if working {
        button_disabled("Add note").h(px(30.)).into_any_element()
    } else {
        button("tiny-rep-add-note", ButtonKind::Secondary, "Add note")
            .h(px(30.))
            .on_click(cx.listener(move |this, _, _, cx| {
                let text = this.tiny_state.settings_fields.report_note.read(cx).text().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                this.tiny_state.settings_fields.report_note.update(cx, |input, cx| input.set_text("", cx));
                this.tiny_report_work(add_id.clone(), TinyReportUpdate { note: Some(text), ..TinyReportUpdate::default() }, cx);
            }))
            .into_any_element()
    };
    notes_box = notes_box.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().child(field(focused).child(div().flex_1().min_w_0().child(note_input.clone()))))
            .child(add_btn),
    );
    body = body.child(notes_box);
    body = body.child(kit::note(
        "icons/info.svg",
        theme::text_faint(),
        "Report contents stay in memory only and are cleared when you close Server Settings.",
    ));
    body
}

impl Shell {
    pub(super) fn tiny_reports_load(&mut self, more: bool, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let Some(overlay) = self.tiny_state.settings.as_mut() else {
            return;
        };
        let draft = &mut overlay.reports;
        let before = if more { draft.rows.last().map(|r| r.id.clone()) } else { None };
        let filter = TinyReportFilter {
            status: draft.status.to_owned(),
            kind: draft.kind.to_owned(),
            before,
            limit: PAGE,
        };
        draft.loading = true;
        draft.asked = true;
        draft.error = None;
        if !more {
            draft.rows.clear();
            draft.end = false;
            draft.epoch += 1;
        }
        let epoch = draft.epoch;
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyReportsList { account, filter }),
            move |this, answer, cx| {
                let Some(overlay) = this.tiny_state.settings.as_mut() else {
                    return;
                };
                if overlay.reports.epoch != epoch {
                    return;
                }
                overlay.reports.loading = false;
                match answer {
                    Ok(Response::TinyReports { reports }) => {
                        overlay.reports.end = (reports.len() as u32) < PAGE;
                        if more {
                            overlay.reports.rows.extend(reports);
                        } else {
                            overlay.reports.rows = reports;
                        }
                    }
                    Ok(Response::Error { detail, .. }) => overlay.reports.error = Some(detail),
                    Ok(_) => overlay.reports.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => overlay.reports.error = Some(crate::client::trouble_words("tiny", &err)),
                }
                cx.notify();
            },
        );
    }

    fn tiny_report_open(&mut self, report_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.reports.reading = true;
            overlay.reports.detail_error = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyReportRead { account, report_id }),
            move |this, answer, cx| {
                let Some(overlay) = this.tiny_state.settings.as_mut() else {
                    return;
                };
                overlay.reports.reading = false;
                match answer {
                    Ok(Response::TinyReport { report }) => {
                        overlay.reports.selected = Some(*report);
                    }
                    Ok(Response::Error { detail, .. }) => overlay.reports.detail_error = Some(detail),
                    Ok(_) => overlay.reports.detail_error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => overlay.reports.detail_error = Some(crate::client::trouble_words("tiny", &err)),
                }
                cx.notify();
            },
        );
    }

    fn tiny_report_work(&mut self, report_id: String, update: TinyReportUpdate, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.reports.working = true;
            overlay.reports.detail_error = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyReportWork { account, report_id, update }),
            move |this, answer, cx| {
                let Some(overlay) = this.tiny_state.settings.as_mut() else {
                    return;
                };
                overlay.reports.working = false;
                match answer {
                    Ok(Response::TinyReport { report }) => {
                        let summary = report.summary.clone();
                        if let Some(row) = overlay.reports.rows.iter_mut().find(|r| r.id == summary.id) {
                            *row = summary;
                        }
                        overlay.reports.selected = Some(*report);
                    }
                    Ok(Response::Error { detail, .. }) => overlay.reports.detail_error = Some(detail),
                    Ok(_) => overlay.reports.detail_error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => overlay.reports.detail_error = Some(crate::client::trouble_words("tiny", &err)),
                }
                cx.notify();
            },
        );
    }
}
