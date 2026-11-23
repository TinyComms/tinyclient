//! Create an online meeting and share its join link in the originating conversation.

use super::{https_host, iso_utc, now_ms, Audience, Auth, Body, Fault, Teams};
use protocol::{
    Card, CardAction, Conversation, ConversationKind, MeetingDraft, MeetingEvent, Outgoing,
};
use serde_json::{json, Value};

impl Teams {
    fn meeting_destination(&self, conversation: &str) -> Result<Conversation, Fault> {
        let row = self
            .ctx
            .known(conversation)?
            .ok_or_else(|| invalid("This conversation is unavailable."))?;
        if row.read_only
            || !matches!(
                row.kind,
                ConversationKind::Direct | ConversationKind::Group | ConversationKind::Channel
            )
        {
            return Err(invalid(
                "Meetings can be shared in writable Teams chats and channels.",
            ));
        }
        Ok(row)
    }

    pub(super) fn meeting_invitees(&self, conversation: &str) -> Result<Vec<String>, Fault> {
        let row = self.meeting_destination(conversation)?;
        let mut emails = Vec::new();
        if row.kind != ConversationKind::Channel {
            let me = self.me();
            for member in self.members(conversation, 0, 51)?.members {
                if member.key.id != me && !member.bot {
                    if let Some(person) = self.cached(&member.key.id) {
                        if valid_email(&person.mail)
                            && !emails
                                .iter()
                                .any(|known: &String| known.eq_ignore_ascii_case(&person.mail))
                        {
                            emails.push(person.mail);
                        }
                    }
                }
            }
        }
        if emails.len() > 50 {
            return Err(invalid(
                "This chat has more than 50 attendees. Enter the people you want to invite.",
            ));
        }
        Ok(emails)
    }

    pub(super) fn create_meeting(
        &self,
        conversation: &str,
        draft: &MeetingDraft,
    ) -> Result<(MeetingEvent, bool), Fault> {
        self.meeting_destination(conversation)?;
        let title = draft.title.trim();
        if title.is_empty() || title.len() > 200 || title.chars().any(char::is_control) {
            return Err(invalid("Enter a meeting title of 200 characters or fewer."));
        }
        let request_id = draft.request_id.as_str();
        if request_id.is_empty()
            || request_id.len() > 64
            || !request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(invalid("Invalid meeting request id."));
        }
        let now = now_ms() / 1000;
        let (path, body, start, end) = if let Some(schedule) = &draft.schedule {
            if schedule.start <= now
                || schedule.end <= schedule.start
                || schedule.end >= 4_102_444_800
            {
                return Err(invalid("Choose a future start and an end after it."));
            }
            if schedule.attendees.len() > 50
                || schedule.attendees.iter().any(|mail| !valid_email(mail))
            {
                return Err(invalid("Enter up to 50 valid attendee email addresses."));
            }
            let graph_time = |at| {
                let utc = iso_utc(at);
                json!({"dateTime": utc.trim_end_matches('Z'), "timeZone": "UTC"})
            };
            let attendees: Vec<_> = schedule
                .attendees
                .iter()
                .map(|mail| {
                    json!({
                        "emailAddress": {"address": mail}, "type": "required",
                    })
                })
                .collect();
            let mut body = json!({
                "subject": title, "start": graph_time(schedule.start), "end": graph_time(schedule.end),
                "isOnlineMeeting": true, "onlineMeetingProvider": "teamsForBusiness",
                "attendees": attendees, "transactionId": request_id,
            });
            if schedule.weekly {
                let weekday = [
                    "thursday",
                    "friday",
                    "saturday",
                    "sunday",
                    "monday",
                    "tuesday",
                    "wednesday",
                ][schedule.start.div_euclid(86_400).rem_euclid(7) as usize];
                let date = iso_utc(schedule.start);
                body["recurrence"] = json!({
                    "pattern": {"type": "weekly", "interval": 1, "daysOfWeek": [weekday], "firstDayOfWeek": "monday"},
                    "range": {"type": "noEnd", "startDate": &date[..10], "recurrenceTimeZone": "UTC"}
                });
            }
            ("/v1.0/me/events", body, schedule.start, schedule.end)
        } else {
            let end = now + 3600;
            (
                "/v1.0/me/onlineMeetings/createOrGet",
                json!({
                    "externalId": request_id, "subject": title, "startDateTime": iso_utc(now), "endDateTime": iso_utc(end),
                }),
                now,
                end,
            )
        };
        let url = format!("{}{path}", self.graph);
        let answer = self
            .call(
                "POST",
                &url,
                Auth::Bearer(Audience::Graph),
                Body::Json(&body),
                false,
            )?
            .body;
        let join = answer["onlineMeeting"]["joinUrl"]
            .as_str()
            .or_else(|| answer["joinWebUrl"].as_str())
            .filter(|join| self.valid_meeting_url(join))
            .ok_or_else(|| {
                invalid(
                    "Teams did not return a join link. Check your calendar before trying again.",
                )
            })?;
        let event = MeetingEvent {
            id: answer["id"].as_str().unwrap_or(request_id).to_owned(),
            title: title.to_owned(),
            start,
            end,
            organizer: self.own_name(),
            join_url: join.to_owned(),
            ..MeetingEvent::default()
        };
        // Instant meetings are independent of the calendar. Scheduled events are reloaded there.
        let posted = self
            .share_meeting(conversation, request_id, &event, draft.schedule.is_some())
            .is_ok();
        Ok((event, posted))
    }

    fn share_meeting(
        &self,
        conversation: &str,
        request_id: &str,
        event: &MeetingEvent,
        scheduled: bool,
    ) -> Result<(), Fault> {
        let invite = json!({
            "title": event.title, "joinUrl": event.join_url, "start": event.start, "end": event.end, "scheduled": scheduled,
        });
        // Supply ordinary rich message text too, so other Teams clients have the link.
        let text = format!("{}\n{}", event.title, event.join_url);
        let options = Outgoing {
            suppress_embeds: true,
            ..Outgoing::default()
        };
        let client_id = if request_id.bytes().all(|b| b.is_ascii_digit()) {
            request_id.to_owned()
        } else {
            now_ms().to_string()
        };
        let mut message = self.posted_with(
            conversation,
            &client_id,
            &text,
            None,
            &options,
            Some(json!({"tinyclientMeeting": invite.to_string()})),
        )?;
        if let Some(card) = self.meeting_invite_card(&invite) {
            message.card = Some(Box::new(card));
        }
        self.ctx.sent(conversation, &client_id, Ok(message));
        Ok(())
    }

    pub(super) fn meeting_invite_card(&self, invite: &Value) -> Option<Card> {
        let decoded = invite
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let invite = decoded.as_ref().unwrap_or(invite);
        let title = invite["title"].as_str().filter(|s| !s.is_empty())?;
        let join = invite["joinUrl"]
            .as_str()
            .filter(|s| self.valid_meeting_url(s))?;
        let scheduled = invite["scheduled"] == true;
        let mut card = Card {
            title: title.to_owned(),
            author: if scheduled {
                "Scheduled meeting"
            } else {
                "Instant meeting"
            }
            .to_owned(),
            time: invite["start"].as_i64().filter(|&at| at > 0),
            actions: vec![CardAction {
                label: "Join meeting".to_owned(),
                url: join.to_owned(),
            }],
            ..Card::default()
        };
        if let (Some(start), Some(end)) = (card.time, invite["end"].as_i64().filter(|&end| end > 0))
        {
            if scheduled && end > start {
                card.fields.push((
                    "Duration".to_owned(),
                    format!("{} min", end.saturating_sub(start) / 60),
                ));
            }
        }
        Some(card)
    }

    fn valid_meeting_url(&self, url: &str) -> bool {
        let teams = https_host(self.cloud.endpoints().teams).unwrap_or_default();
        let Ok(uri) = url.parse::<ureq::http::Uri>() else {
            return false;
        };
        https_host(url) == Some(teams)
            && uri
                .authority()
                .is_some_and(|authority| authority.as_str() == teams)
            && (uri.path().starts_with("/l/meetup-join/") || uri.path().starts_with("/meet/"))
    }
}

fn valid_email(mail: &str) -> bool {
    let Some((local, domain)) = mail.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.contains('@')
        && mail.len() <= 254
        && !mail
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, ',' | ';' | '<' | '>'))
}

fn invalid(message: &str) -> Fault {
    Fault::Refused {
        status: 400,
        message: message.to_owned(),
    }
}
