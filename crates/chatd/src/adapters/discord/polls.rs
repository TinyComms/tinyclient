//! Discord polls use the shared poll model. Discord answer IDs survive edits and are translated
//! at the adapter boundary, so the rest of the app can keep addressing choices by position.

use super::super::{Failure, ReactorsPage, Refusal};
use super::{model, refusal, Discord};
use protocol::{MediaFile, Poll, PollChoice, PollDraft, Reactor, POLL_OPTIONS};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(super) struct VoteEvent {
    channel_id: String,
    message_id: String,
    user_id: String,
    answer_id: u32,
}

#[derive(Deserialize)]
struct Voters {
    users: Vec<model::User>,
}

pub(super) fn convert(wire: &model::Poll, previous: Option<&Poll>) -> (Option<Poll>, Vec<(MediaFile, String)>) {
    let mut files = Vec::new();
    let question = wire.question.as_ref().and_then(|question| question.text.clone())
        .or_else(|| previous.map(|poll| poll.question.clone())).unwrap_or_default();
    let mut poll = previous.cloned().unwrap_or_else(|| Poll::open(&question, std::iter::empty::<String>(), false));
    poll.question = question;
    if let Some(multiple) = wire.allow_multiselect {
        poll.multiple = multiple;
    }
    if let Some(expiry) = &wire.expiry {
        poll.closes_at = model::iso_seconds(expiry);
    }
    if let Some(answers) = &wire.answers {
        poll.choices = answers.iter().take(POLL_OPTIONS).map(|answer| {
            let (emoji, file) = answer.poll_media.emoji.as_ref().map(model::Emoji::spelled).unwrap_or_default();
            let emoji_file = file.map(|entry| {
                let file = entry.0.clone();
                files.push(entry);
                file
            });
            let count = previous.and_then(|poll| poll.choices.iter().find(|choice| choice.answer_id == Some(answer.answer_id)))
                .map_or(0, |choice| choice.count);
            PollChoice {
                text: answer.poll_media.text.clone().unwrap_or_default(),
                count,
                answer_id: Some(answer.answer_id),
                emoji,
                emoji_file,
                ..PollChoice::default()
            }
        }).collect();
    }
    if let Some(results) = &wire.results {
        poll.closed = results.is_finalized;
        poll.mine.clear();
        for (index, choice) in poll.choices.iter_mut().enumerate() {
            let reported = results.answer_counts.iter().find(|count| Some(count.id) == choice.answer_id);
            choice.count = reported.map_or(0, |count| count.count);
            if reported.is_some_and(|count| count.me_voted) {
                poll.mine.push(index as u32);
            }
        }
    }
    ((!poll.choices.is_empty()).then_some(poll), files)
}

pub(super) fn create_body(draft: &PollDraft) -> Result<Value, String> {
    let question = draft.question.trim();
    if !(1..=300).contains(&question.chars().count()) {
        return Err("A Discord poll question must be 1–300 characters.".into());
    }
    if !(2..=POLL_OPTIONS).contains(&draft.options.len()) {
        return Err("Add 2–10 poll answers.".into());
    }
    let mut answers = Vec::with_capacity(draft.options.len());
    for option in &draft.options {
        let option = option.trim();
        let custom = option.find('>').and_then(|end| {
            protocol::discord_emoji_token(&option[..=end]).map(|(_, id, _)| (id, option[end + 1..].trim()))
        });
        let text = custom.map_or(option, |(_, text)| text);
        if !(1..=55).contains(&text.chars().count()) {
            return Err("Each Discord poll answer must be 1–55 characters.".into());
        }
        let mut media = json!({"text":text});
        if let Some((id, _)) = custom {
            media["emoji"] = json!({"id":id});
        }
        answers.push(json!({"poll_media":media}));
    }
    let duration = draft.duration_secs.unwrap_or(24 * 3600);
    if !(3600..=7 * 24 * 3600).contains(&duration) || duration % 3600 != 0 || draft.anonymous {
        return Err("Discord polls run for 1–168 whole hours and show who voted.".into());
    }
    Ok(json!({
        "question":{"text":question}, "answers":answers, "duration":duration / 3600,
        "allow_multiselect":draft.multiple, "layout_type":1
    }))
}

fn answer_id(poll: &Poll, index: u32) -> Result<u32, Refusal> {
    poll.choices.get(index as usize).and_then(|choice| choice.answer_id)
        .ok_or_else(|| Refusal::Refused("That poll answer is no longer available.".into()))
}

impl Discord {
    pub(super) fn poll_vote_event(&self, event: VoteEvent, added: bool) -> Result<(), Failure> {
        let Some(mut message) = self.ctx.stored_message(&event.channel_id, &event.message_id)? else {
            return Ok(());
        };
        let Some(poll) = message.poll.as_mut().filter(|poll| !poll.closed) else { return Ok(()) };
        let Some((index, choice)) = poll.choices.iter_mut().enumerate()
            .find(|(_, choice)| choice.answer_id == Some(event.answer_id)) else { return Ok(()) };
        if event.user_id == self.me() {
            let index = index as u32;
            if added == poll.mine.contains(&index) {
                return Ok(());
            }
            if added {
                poll.mine.push(index);
                poll.mine.sort_unstable();
            } else {
                poll.mine.retain(|id| *id != index);
            }
        }
        choice.count = if added { choice.count.saturating_add(1) } else { choice.count.saturating_sub(1) };
        self.ctx.message(&event.channel_id, &message)?;
        Ok(())
    }

    pub(super) fn vote_poll(&self, channel: &str, message_id: &str, votes: Option<Vec<u32>>) -> Result<(), Refusal> {
        let message = self.ctx.stored_message(channel, message_id).map_err(|err| Refusal::Refused(err.to_string()))?;
        let Some(message) = message.filter(|message| message.poll.is_some()) else {
            return Err(Refusal::Refused("This poll is no longer available.".into()));
        };
        let poll = message.poll.as_ref().expect("checked poll");
        if poll.closed || poll.closes_at.is_some_and(|expiry| expiry <= crate::hub::now()) {
            return Err(Refusal::Refused("This poll has closed.".into()));
        }
        let route = format!("/channels/{channel}/polls/{message_id}");
        if let Some(mut votes) = votes {
            votes.sort_unstable();
            votes.dedup();
            if !poll.multiple && votes.len() > 1 {
                return Err(Refusal::Refused("Choose one answer in this poll.".into()));
            }
            let answers: Vec<_> = votes.into_iter().map(|index| answer_id(poll, index)).collect::<Result<_, _>>()?;
            self.call("PUT", &format!("{route}/answers/@me"), Some(json!({"answer_ids": answers}))).map_err(refusal)?;
        } else {
            if !message.outgoing {
                return Err(Refusal::Refused("Only the poll's author can end it.".into()));
            }
            let raw = self.call("POST", &format!("{route}/expire"), None).map_err(refusal)?;
            let posted = serde_json::from_str(&raw).map_err(|_| Refusal::Refused("Discord did not return the ended poll.".into()))?;
            self.updated(posted).map_err(super::manage::failed)?;
        }
        Ok(())
    }

    pub(super) fn poll_voters(&self, channel: &str, message_id: &str, choice: u32, after: Option<&str>) -> Result<ReactorsPage, Refusal> {
        const LIMIT: usize = 25;
        let poll = self.ctx.stored_message(channel, message_id).map_err(|err| Refusal::Refused(err.to_string()))?
            .and_then(|message| message.poll).ok_or_else(|| Refusal::Refused("This poll is no longer available.".into()))?;
        let id = answer_id(&poll, choice)?;
        let mut route = format!("/channels/{channel}/polls/{message_id}/answers/{id}?limit={LIMIT}");
        if let Some(after) = after.filter(|after| !after.is_empty() && after.len() <= 20 && after.bytes().all(|b| b.is_ascii_digit())) {
            route.push_str(&format!("&after={after}"));
        }
        let raw = self.call("GET", &route, None).map_err(refusal)?;
        let page: Voters = serde_json::from_str(&raw).map_err(|_| Refusal::Refused("Discord did not return the poll voters.".into()))?;
        let more = page.users.len() >= LIMIT;
        let reactors = page.users.into_iter().take(LIMIT).map(|user| Reactor {
            key: self.ctx.key(&user.id), name: user.name().to_owned(), avatar: None,
        }).collect();
        Ok(ReactorsPage { reactors, more })
    }
}
