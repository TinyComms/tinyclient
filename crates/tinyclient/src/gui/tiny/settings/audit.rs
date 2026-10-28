//! Audit log: still to be drawn. What it holds: The audit as sentences with names ("Mara made Leila a moderator"), an icon per kind, relative and exact times, filters by person and by kind, and older pages as it scrolls. Request: TinyAudit with a TinyAuditFilter.
//!
//! The frame (`settings/mod.rs`) lists it where `Page::Audit.visible` says, and calls the
//! function below with the page as the server last said it. Its draft is [`Draft`], in
//! `Overlay::audit`; when it edits, give it `dirty`, `save` and `reset` and add them to the
//! frame's `tiny_settings_dirty`, `tiny_settings_save` and `tiny_settings_reset`.

use super::{coming, Page};
use crate::gui::state::Shell;
use crate::gui::tiny::Page as Facts;
use gpui::{AnyElement, Context, Window};

// The sentences of the audit as the page before this one drew them, kept for the Audit page,
// which adds the newer actions and the names the server now sends (`actor_name`).
#[allow(dead_code)]
mod words {
    /// A day from `YYYY-MM-DD…`. Anything else is left as the server sent it.
    pub(in crate::gui) fn short_day(stamp: &str) -> String {
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
        if role.eq_ignore_ascii_case("admin") {
            "an admin"
        } else if role.eq_ignore_ascii_case("member") {
            "a member"
        } else if role.eq_ignore_ascii_case("owner") {
            "an owner"
        } else {
            ""
        }
    }

    /// What one audit row says. Machine actions stay off the screen.
    pub(in crate::gui) fn audit_line<'a>(action: &str, detail: &str, name_of: impl Fn(&str) -> Option<&'a str>) -> Option<String> {
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
}
#[allow(unused_imports)]
pub(in crate::gui) use words::{audit_line, short_day};

/// What has changed on the page and not been saved.
#[derive(Default)]
pub(in crate::gui) struct Draft {}

pub(in crate::gui::tiny) fn audit(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (shell, facts, window, cx);
    coming(Page::Audit, "What was done on this server, by whom and why.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_audit_row_is_a_sentence_and_a_stamp_is_a_day() {
        assert_eq!(audit_line("bootstrap", "", |_| None).as_deref(), Some("This server was created."));
        assert_eq!(audit_line("invite.create", r#"{"role":"admin"}"#, |_| None).as_deref(), Some("An invite for an admin was created."));
        assert_eq!(audit_line("invite.revoke", "{}", |_| None).as_deref(), Some("An invite was revoked."));
        assert_eq!(audit_line("invite.accept", r#"{"role":"member"}"#, |_| None).as_deref(), Some("A member joined."));
        assert_eq!(audit_line("member.role", r#"{"user_id":"a","role":"admin"}"#, |id| (id == "a").then_some("Ada")).as_deref(), Some("Ada is now an admin."));
        assert_eq!(audit_line("member.remove", r#"{"user_id":"gone"}"#, |_| None).as_deref(), Some("Someone was removed."));
        assert_eq!(audit_line("nope", "membership", |_| None), None);
        assert_eq!(short_day("2026-11-01T00:00:00Z"), "Nov 1, 2026");
        assert_eq!(short_day("soon"), "soon");
    }
}
