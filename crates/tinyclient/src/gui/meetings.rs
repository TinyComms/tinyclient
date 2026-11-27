//! Create or schedule a Teams meeting in the current chat or channel.

use super::components::{
    accessible_modal, button, button_disabled, field_on, icon_button, modal_body, modal_buttons,
    modal_layer, tip, ButtonKind,
};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use crate::{client::ask, view::now};
use gpui::{
    div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, Entity, Focusable, MouseButton,
    Stateful, Subscription, Window,
};
use protocol::{
    Conversation, ConversationKind, Key, Link, MeetingDraft, MeetingEvent, MeetingSchedule,
    Network, Request, Response,
};

pub(super) struct Form {
    conversation: Key,
    place: String,
    channel: bool,
    id: String,
    title: Entity<TextInput>,
    start: Entity<TextInput>,
    end: Entity<TextInput>,
    attendees: Entity<TextInput>,
    scheduled: bool,
    weekly: bool,
    loading_attendees: bool,
    busy: bool,
    error: String,
    attendee_note: String,
    created: Option<(MeetingEvent, bool)>,
    focused: Option<usize>,
    _focus_subs: Vec<Subscription>,
}

impl Shell {
    pub(super) fn meeting_button(
        &self,
        row: &Conversation,
        link: Link,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if row.key.network != Network::Teams
            || link != Link::Connected
            || row.read_only
            || !matches!(
                row.kind,
                ConversationKind::Direct | ConversationKind::Group | ConversationKind::Channel
            )
        {
            return None;
        }
        let to = row.clone();
        let control = if compact {
            icon_button("meeting-menu", "icons/calendar.svg", false)
                .when(super::phone::active(), |button| {
                    button.size(px(super::phone::tokens::TOUCH))
                })
        } else {
            button("meeting-menu", ButtonKind::Ghost, "Meet")
        };
        Some(
            control
                .flex_none()
                .when(!super::phone::active(), |button| button.ml(px(8.)))
                .aria_label("Teams meeting")
                .tooltip(tip("Meet now or schedule a meeting", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_meeting(&to, window, cx)),
                ),
        )
    }

    fn open_meeting(&mut self, row: &Conversation, window: &mut Window, cx: &mut Context<Self>) {
        let input = |placeholder, text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut field = TextInput::new(placeholder, cx);
                field.set_text(text, cx);
                field
            })
        };
        let start = (now() / 900 + 2) * 900;
        let title = input("Meeting title", &format!("Meeting in {}", row.title), cx);
        let start_input = input("YYYY-MM-DD HH:MM", &local_input(start), cx);
        let end_input = input("YYYY-MM-DD HH:MM", &local_input(start + 1800), cx);
        let attendees = input("Email addresses separated by commas", "", cx);
        let mut focus_subs = Vec::with_capacity(8);
        for (idx, entity) in [(0, &title), (1, &start_input), (2, &end_input), (3, &attendees)] {
            let handle = entity.focus_handle(cx);
            focus_subs.push(cx.on_focus_in(&handle, window, move |this, _, cx| {
                if let Some(form) = &mut this.meeting_form {
                    form.focused = Some(idx);
                    cx.notify();
                }
            }));
            focus_subs.push(cx.on_focus_out(&handle, window, move |this, _, _, cx| {
                if let Some(form) = &mut this.meeting_form {
                    if form.focused == Some(idx) {
                        form.focused = None;
                        cx.notify();
                    }
                }
            }));
        }
        window.focus(&title.read(cx).focus_handle(cx), cx);
        self.meeting_form = Some(Form {
            conversation: row.key.clone(),
            place: row.title.clone(),
            channel: row.kind == ConversationKind::Channel,
            id: super::composer::client_id(),
            title,
            start: start_input,
            end: end_input,
            attendees,
            scheduled: false,
            weekly: false,
            loading_attendees: false,
            busy: false,
            error: String::new(),
            attendee_note: String::new(),
            created: None,
            focused: Some(0),
            _focus_subs: focus_subs,
        });
        cx.notify();
    }

    fn show_meeting_schedule(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.meeting_form else {
            return;
        };
        form.scheduled = true;
        form.error.clear();
        cx.notify();
        if form.channel {
            return;
        }
        form.loading_attendees = true;
        let request = Request::MeetingInvitees {
            conversation: form.conversation.clone(),
        };
        let id = form.id.clone();
        self.off_thread(
            cx,
            move || ask(&request),
            move |this, answer, cx| {
                if let Some(form) = this.meeting_form.as_mut().filter(|form| form.id == id) {
                    form.loading_attendees = false;
                    match answer {
                        Ok(Response::MeetingInvitees { emails }) => {
                            if form.attendees.read(cx).text().is_empty() {
                                form.attendees
                                    .update(cx, |field, cx| field.set_text(&emails.join(", "), cx));
                            }
                        }
                        Ok(Response::Error { detail, .. }) => form.attendee_note = detail,
                        _ => {
                            form.attendee_note =
                                "Enter the email addresses of the people you want to invite."
                                    .to_owned()
                        }
                    }
                    cx.notify();
                }
            },
        );
    }

    fn confirm_meeting(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.meeting_form else {
            return;
        };
        if form.busy || form.loading_attendees || form.created.is_some() {
            return;
        }
        let draft = meeting_draft(
            &form.id,
            form.title.read(cx).text(),
            form.scheduled.then(|| {
                (
                    form.start.read(cx).text(),
                    form.end.read(cx).text(),
                    form.attendees.read(cx).text(),
                    form.weekly,
                )
            }),
            now(),
        );
        let draft = match draft {
            Ok(draft) => draft,
            Err(error) => {
                form.error = error.to_owned();
                cx.notify();
                return;
            }
        };
        form.busy = true;
        form.error.clear();
        let id = form.id.clone();
        let request = Request::CreateMeeting {
            conversation: form.conversation.clone(),
            draft,
        };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            if let Some(form) = this.meeting_form.as_mut().filter(|form| form.id == id) {
                form.busy = false;
                match answer {
                    Ok(Response::MeetingCreated { event, posted }) => form.created = Some((event, posted)),
                    Ok(Response::Error { detail, .. }) => form.error = detail,
                    _ => form.error = "Teams could not confirm creation. Check your calendar before retrying.".to_owned(),
                }
                cx.notify();
            }
        });
        cx.notify();
    }

    pub(super) fn close_meeting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.meeting_form.as_ref().is_some_and(|form| !form.busy) {
            self.meeting_form = None;
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    pub(super) fn meeting_layer(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let form = self.meeting_form.as_ref()?;
        let title = if form.created.is_some() {
            "Meeting created"
        } else if form.scheduled {
            "Schedule meeting"
        } else {
            "Teams meeting"
        };
        let panel = accessible_modal(title)
            .max_w_full()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_meeting(window, cx);
                }
            }));
        let cancel = |label| {
            button("meeting-close", ButtonKind::Ghost, label)
                .on_click(cx.listener(|this, _, window, cx| this.close_meeting(window, cx)))
        };
        let panel = if let Some((event, posted)) = &form.created {
            let copy = event.join_url.clone();
            let open = event.join_url.clone();
            panel
                .child(modal_body(event.title.clone()))
                .children((!*posted).then(|| {
                    modal_body(
                        "The meeting was created. Copy the link to share it in the conversation.",
                    )
                }))
                .child(
                    modal_buttons()
                        .child(cancel("Done"))
                        .child(
                            button("meeting-copy", ButtonKind::Secondary, "Copy link").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    this.toast(None, "Meeting link copied", cx);
                                }),
                            ),
                        )
                        .child(
                            button("meeting-open", ButtonKind::Primary, "Open in Teams")
                                .on_click(move |_, _, cx| cx.open_url(&open)),
                        ),
                )
        } else {
            let mut fields = div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .mt(px(12.))
                .child(field("Title", &form.title, form.focused == Some(0)));
            if form.scheduled {
                fields = fields
                    .child(field("Start · local time", &form.start, form.focused == Some(1)))
                    .child(field("End · local time", &form.end, form.focused == Some(2)))
                    .child(field("Attendee emails", &form.attendees, form.focused == Some(3)))
                    .children(
                        form.loading_attendees
                            .then(|| modal_body("Loading chat members…")),
                    )
                    .children(
                        (!form.attendee_note.is_empty())
                            .then(|| modal_body(form.attendee_note.clone())),
                    )
                    .child(
                        div()
                            .id("meeting-weekly")
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .py(px(4.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(form) = &mut this.meeting_form {
                                    form.weekly = !form.weekly;
                                    cx.notify();
                                }
                            }))
                            .child(super::settings::controls::switch(
                                "meeting-weekly-switch",
                                "Repeat weekly",
                                form.weekly,
                            ))
                            .child(
                                div()
                                    .type_ui()
                                    .text_color(theme::text())
                                    .child("Repeat weekly"),
                            ),
                    );
            }
            let action = if form.scheduled {
                "Confirm and schedule"
            } else {
                "Meet now"
            };
            let confirm = if form.busy || form.loading_attendees {
                button_disabled(if form.busy { "Creating…" } else { action }).into_any_element()
            } else {
                button("meeting-confirm", ButtonKind::Primary, action)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_meeting(cx)))
                    .into_any_element()
            };
            let place = if form.scheduled {
                format!("Calendar invitations go to the addresses below. The meeting link is posted in {}.", form.place)
            } else {
                format!("Create a meeting and post its link in {}.", form.place)
            };
            panel
                .child(modal_body(place))
                .child(fields)
                .children((!form.error.is_empty()).then(|| {
                    div()
                        .mt(px(8.))
                        .type_ui()
                        .text_color(theme::danger())
                        .child(form.error.clone())
                }))
                .child(
                    modal_buttons()
                        .children((!form.busy).then(|| cancel("Cancel")))
                        .children((!form.scheduled && !form.busy).then(|| {
                            button("meeting-schedule", ButtonKind::Secondary, "Schedule…").on_click(
                                cx.listener(|this, _, _, cx| this.show_meeting_schedule(cx)),
                            )
                        }))
                        .child(confirm),
                )
        };
        Some(modal_layer(
            "meeting-scrim",
            window,
            panel,
            cx.listener(|this, _, window, cx| this.close_meeting(window, cx)),
        ))
    }
}

fn field(label: &'static str, input: &Entity<TextInput>, focused: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(
            div()
                .type_small()
                .text_color(theme::text_soft())
                .child(label),
        )
        .child(field_on(focused, theme::overlay()).child(div().flex_1().min_w_0().child(input.clone())))
}

fn local_input(unix: i64) -> String {
    let local = super::time::local(unix);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.year,
        local.month + 1,
        local.day,
        local.hour,
        local.minute
    )
}

fn input_time(value: &str) -> Option<i64> {
    let value = value.trim();
    let (date, time) = value.split_once(' ')?;
    let (year, rest) = date.split_once('-')?;
    let (month, day) = rest.split_once('-')?;
    let (hour, minute) = time.split_once(':')?;
    let (year, month, day, hour, minute) = (
        year.parse().ok()?,
        month.parse().ok()?,
        day.parse().ok()?,
        hour.parse().ok()?,
        minute.parse().ok()?,
    );
    if !(1970..2100).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
    {
        return None;
    }
    let unix = super::composer::schedule::unix_at(year, month, day, hour, minute)?;
    (local_input(unix) == value).then_some(unix)
}

fn meeting_draft(
    id: &str,
    title: &str,
    fields: Option<(&str, &str, &str, bool)>,
    now: i64,
) -> Result<MeetingDraft, &'static str> {
    let title = title.trim();
    if title.is_empty() || title.len() > 200 {
        return Err("Enter a meeting title of 200 characters or fewer.");
    }
    let schedule = if let Some((start, end, emails, weekly)) = fields {
        let start = input_time(start).ok_or("Enter the start as YYYY-MM-DD HH:MM.")?;
        let end = input_time(end).ok_or("Enter the end as YYYY-MM-DD HH:MM.")?;
        if start <= now || end <= start {
            return Err("Choose a future start and an end after it.");
        }
        let mut attendees: Vec<String> = Vec::new();
        for email in emails
            .split([',', ';'])
            .map(str::trim)
            .filter(|email| !email.is_empty())
        {
            if !attendees
                .iter()
                .any(|known| known.eq_ignore_ascii_case(email))
            {
                attendees.push(email.to_owned());
            }
        }
        Some(MeetingSchedule {
            start,
            end,
            attendees,
            weekly,
        })
    } else {
        None
    };
    Ok(MeetingDraft {
        title: title.to_owned(),
        request_id: id.to_owned(),
        schedule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_form_preserves_the_start_end_and_distinct_invitees() {
        let schedule = meeting_draft(
            "01",
            " Design review ",
            Some((
                "2030-10-05 09:00",
                "2030-10-05 09:30",
                "ana@example.test, ANA@example.test; ben@example.test",
                true,
            )),
            0,
        )
        .unwrap()
        .schedule
        .unwrap();
        assert_eq!(schedule.end - schedule.start, 1800);
        assert_eq!(schedule.attendees, ["ana@example.test", "ben@example.test"]);
        assert!(schedule.weekly);
    }

    #[test]
    fn bad_dates_and_reversed_times_do_not_become_meeting_requests() {
        assert!(input_time("2030-02-30 09:00").is_none());
        assert!(meeting_draft(
            "01",
            "Review",
            Some(("2030-10-05 10:00", "2030-10-05 09:00", "", false)),
            0
        )
        .is_err());
        assert!(meeting_draft("02", "Review", None, 0)
            .unwrap()
            .schedule
            .is_none());
    }
}
