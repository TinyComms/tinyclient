//! Messages sent while their account was offline. chatd refuses those outright and stores
//! nothing, so the shell holds each one for a minute: it goes if the link comes back in that
//! time, and after it never goes unless the user asks. Also here: which sends this window made
//! are still on their way, so a conversation whose message failed can say so in the sidebar.
//!
//! Nothing here reads a clock or sends anything. The caller passes `now` and makes the request.

use protocol::{Key, Link};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a message waits for its network. Past this it is never sent on its own.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Messages held at once, over every conversation. One more is refused, not queued.
pub const HELD: usize = 32;
/// Conversations whose sends are followed, and sends followed in each.
const FOLLOWED: usize = 64;
const EACH: usize = 16;

#[derive(Debug, Clone, PartialEq)]
enum Stage {
    Waiting,
    /// Handed to chatd; its answer has not come.
    Sending,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub conversation: Key,
    pub client_id: String,
    /// What the user typed, as the row shows it.
    pub text: String,
    /// The same with mentions in the network's form: what is sent.
    pub wire: String,
    pub reply_to: Option<String>,
    /// Unix seconds of the Return, for the row's place and time.
    pub since: i64,
    /// When the wait began: the Return, or the last Retry.
    at: Instant,
    stage: Stage,
}

impl Held {
    pub fn new(conversation: Key, client_id: String, text: String, wire: String, reply_to: Option<String>, since: i64, now: Instant) -> Held {
        Held { conversation, client_id, text, wire, reply_to, since, at: now, stage: Stage::Waiting }
    }

    pub fn failed(&self) -> Option<&str> {
        match &self.stage {
            Stage::Failed(reason) => Some(reason),
            _ => None,
        }
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.stage = Stage::Failed(reason.into());
    }
}

fn gave_up(conversation: &Key) -> String {
    format!("{} did not come back.", conversation.network.label())
}

#[derive(Default)]
pub struct Hold {
    entries: Vec<Held>,
}

impl Hold {
    /// Starts the wait. False when too much is held already: the caller keeps the text.
    pub fn hold(&mut self, mut held: Held, now: Instant) -> bool {
        if self.entries.len() >= HELD {
            return false;
        }
        held.at = now;
        held.stage = Stage::Waiting;
        self.entries.push(held);
        true
    }

    pub fn of<'a>(&'a self, conversation: &'a Key) -> impl Iterator<Item = &'a Held> {
        self.entries.iter().filter(move |held| held.conversation == *conversation)
    }

    pub fn has(&self, conversation: &Key, client_id: &str) -> bool {
        self.of(conversation).any(|held| held.client_id == client_id)
    }

    pub fn failed_in(&self, conversation: &Key) -> bool {
        self.of(conversation).any(|held| held.failed().is_some())
    }

    /// Fails in place whatever has waited the whole window. True when anything changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for held in &mut self.entries {
            if held.stage == Stage::Waiting && now.saturating_duration_since(held.at) >= WINDOW {
                held.fail(gave_up(&held.conversation));
                changed = true;
            }
        }
        changed
    }

    /// How long until the next waiting message runs out of time. `None`: no timer is needed.
    pub fn next_expiry(&self, now: Instant) -> Option<Duration> {
        let waiting = self.entries.iter().filter(|held| held.stage == Stage::Waiting);
        waiting.map(|held| (held.at + WINDOW).saturating_duration_since(now)).min()
    }

    /// The links changed. Returns what to send now: everything still inside its window whose
    /// account is connected. A message whose account signed out is dropped with the account;
    /// one whose session the network ended fails in place.
    pub fn linked(&mut self, now: Instant, link_of: impl Fn(&Key) -> Link) -> Vec<Held> {
        self.expire(now);
        self.entries.retain(|held| link_of(&held.conversation) != Link::SignedOut);
        let mut ready = Vec::new();
        for held in &mut self.entries {
            match (link_of(&held.conversation), &held.stage) {
                (Link::Expired, Stage::Waiting) => held.fail(format!("{} signed you out.", held.conversation.network.label())),
                (Link::Connected, Stage::Waiting) => {
                    held.stage = Stage::Sending;
                    ready.push(held.clone());
                }
                _ => {}
            }
        }
        ready
    }

    /// chatd took it: its own pending row stands for the message from here on.
    pub fn sent(&mut self, conversation: &Key, client_id: &str) {
        self.entries.retain(|held| !(held.conversation == *conversation && held.client_id == client_id));
    }

    /// chatd did not take it. Offline again inside the window is more waiting; anything else,
    /// or too late, is the end.
    pub fn refused(&mut self, conversation: &Key, client_id: &str, offline: bool, reason: &str, now: Instant) {
        let Some(held) = self.entries.iter_mut().find(|held| held.conversation == *conversation && held.client_id == client_id) else {
            return;
        };
        if offline && now.saturating_duration_since(held.at) < WINDOW {
            held.stage = Stage::Waiting;
        } else if offline {
            held.fail(gave_up(conversation));
        } else {
            held.fail(reason);
        }
    }

    /// The user asked again. Connected: it is returned to be sent now. Otherwise it waits
    /// another window from `now`.
    pub fn retry(&mut self, conversation: &Key, client_id: &str, connected: bool, now: Instant) -> Option<Held> {
        let held = self.entries.iter_mut().find(|held| held.conversation == *conversation && held.client_id == client_id)?;
        if held.stage == Stage::Sending {
            return None;
        }
        held.at = now;
        held.stage = if connected { Stage::Sending } else { Stage::Waiting };
        connected.then(|| held.clone())
    }

    pub fn discard(&mut self, conversation: &Key, client_id: &str) -> bool {
        let before = self.entries.len();
        self.sent(conversation, client_id);
        self.entries.len() != before
    }
}

/// What became of a send chatd stored, as its conversation's newest messages show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Sent,
    Failed,
    /// Not among the newest messages any more.
    Gone,
}

/// The sends chatd stored for this window, per conversation, until each is known to have gone
/// or failed.
#[derive(Default)]
pub struct Flight {
    sending: HashMap<Key, Vec<String>>,
    failed: HashMap<Key, Vec<String>>,
}

impl Flight {
    fn add(map: &mut HashMap<Key, Vec<String>>, conversation: &Key, client_id: &str) {
        if map.len() >= FOLLOWED && !map.contains_key(conversation) {
            map.clear();
        }
        let ids = map.entry(conversation.clone()).or_default();
        if !ids.iter().any(|id| id == client_id) {
            if ids.len() >= EACH {
                ids.remove(0);
            }
            ids.push(client_id.to_owned());
        }
    }

    fn remove(map: &mut HashMap<Key, Vec<String>>, conversation: &Key, client_id: &str) {
        if let Some(ids) = map.get_mut(conversation) {
            ids.retain(|id| id != client_id);
            if ids.is_empty() {
                map.remove(conversation);
            }
        }
    }

    /// chatd stored it as pending, or the user asked for a failed one again.
    pub fn started(&mut self, conversation: &Key, client_id: &str) {
        Self::remove(&mut self.failed, conversation, client_id);
        Self::add(&mut self.sending, conversation, client_id);
    }

    pub fn discarded(&mut self, conversation: &Key, client_id: &str) {
        Self::remove(&mut self.sending, conversation, client_id);
        Self::remove(&mut self.failed, conversation, client_id);
    }

    /// Sends are on their way here, so a `Messages` change is worth one read.
    pub fn in_flight(&self, conversation: &Key) -> bool {
        self.sending.contains_key(conversation)
    }

    pub fn failed_in(&self, conversation: &Key) -> bool {
        self.failed.contains_key(conversation)
    }

    /// What the newest messages say of each send on its way. A send that went clears the
    /// conversation's failures: the user has moved on. True when the mark changed.
    pub fn settle(&mut self, conversation: &Key, outcome_of: impl Fn(&str) -> Outcome) -> bool {
        let before = self.failed_in(conversation);
        for client_id in self.sending.get(conversation).cloned().unwrap_or_default() {
            match outcome_of(&client_id) {
                Outcome::Pending => {}
                Outcome::Gone => Self::remove(&mut self.sending, conversation, &client_id),
                Outcome::Sent => {
                    Self::remove(&mut self.sending, conversation, &client_id);
                    self.failed.remove(conversation);
                }
                Outcome::Failed => {
                    Self::remove(&mut self.sending, conversation, &client_id);
                    Self::add(&mut self.failed, conversation, &client_id);
                }
            }
        }
        before != self.failed_in(conversation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Network;

    fn key(id: &str) -> Key {
        Key { network: Network::Slack, account: "T1".into(), id: id.into() }
    }

    fn held(conversation: &str, client_id: &str) -> Held {
        Held::new(key(conversation), client_id.into(), "Back in ten".into(), "Back in ten".into(), None, 1_700_000_000, Instant::now())
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_held_message_goes_when_the_link_returns_inside_the_window() {
        let start = Instant::now();
        let mut hold = Hold::default();
        assert!(hold.hold(held("c", "1"), start));
        assert_eq!(hold.next_expiry(start + secs(10)), Some(secs(50)));
        // Still offline: nothing to send, still waiting.
        assert!(hold.linked(start + secs(20), |_| Link::Offline).is_empty());
        let ready = hold.linked(start + secs(59), |_| Link::Connected);
        assert_eq!(ready.iter().map(|held| held.client_id.as_str()).collect::<Vec<_>>(), ["1"]);
        // It is asked of chatd once, however often the links are reported.
        assert!(hold.linked(start + secs(59), |_| Link::Connected).is_empty());
        assert_eq!(hold.next_expiry(start + secs(59)), None);
        // The row stays until chatd has its own.
        assert!(hold.has(&key("c"), "1"));
        hold.sent(&key("c"), "1");
        assert!(!hold.has(&key("c"), "1"));
    }

    #[test]
    fn at_sixty_seconds_it_fails_in_place_and_is_never_sent_after() {
        let start = Instant::now();
        let mut hold = Hold::default();
        hold.hold(held("c", "1"), start);
        assert!(!hold.expire(start + secs(59)));
        assert!(hold.expire(start + secs(60)));
        assert!(hold.failed_in(&key("c")));
        assert_eq!(hold.of(&key("c")).next().and_then(Held::failed), Some("Slack did not come back."));
        assert_eq!(hold.next_expiry(start + secs(60)), None);
        // The link coming back an hour later sends nothing.
        assert!(hold.linked(start + secs(3600), |_| Link::Connected).is_empty());
        assert!(hold.failed_in(&key("c")));
    }

    #[test]
    fn a_link_that_returns_too_late_sends_nothing_even_before_the_timer_ran() {
        let start = Instant::now();
        let mut hold = Hold::default();
        hold.hold(held("c", "1"), start);
        assert!(hold.linked(start + secs(61), |_| Link::Connected).is_empty());
        assert!(hold.failed_in(&key("c")));
    }

    #[test]
    fn retry_sends_now_when_connected_and_waits_another_window_when_not() {
        let start = Instant::now();
        let mut hold = Hold::default();
        hold.hold(held("c", "1"), start);
        hold.expire(start + secs(60));
        let later = start + secs(600);
        assert_eq!(hold.retry(&key("c"), "1", false, later), None);
        assert!(!hold.failed_in(&key("c")));
        assert_eq!(hold.next_expiry(later), Some(WINDOW));
        assert!(hold.expire(later + secs(60)));
        let sent = hold.retry(&key("c"), "1", true, later + secs(90));
        assert_eq!(sent.map(|held| held.client_id), Some("1".to_owned()));
        // Asked twice in a row, it is still sent once.
        assert_eq!(hold.retry(&key("c"), "1", true, later + secs(90)), None);
        assert!(hold.discard(&key("c"), "1"));
        assert!(!hold.discard(&key("c"), "1"));
    }

    #[test]
    fn a_refusal_waits_on_inside_the_window_and_is_final_outside_it() {
        let start = Instant::now();
        let mut hold = Hold::default();
        hold.hold(held("c", "1"), start);
        hold.hold(held("c", "2"), start);
        hold.linked(start + secs(5), |_| Link::Connected);
        // The link dropped again before chatd took it.
        hold.refused(&key("c"), "1", true, "", start + secs(6));
        assert_eq!(hold.next_expiry(start + secs(6)), Some(secs(54)));
        hold.refused(&key("c"), "2", false, "You can't post in this channel.", start + secs(6));
        assert_eq!(hold.of(&key("c")).map(Held::failed).collect::<Vec<_>>(), [None, Some("You can't post in this channel.")]);
        hold.linked(start + secs(30), |_| Link::Connected);
        hold.refused(&key("c"), "1", true, "", start + secs(61));
        assert!(hold.of(&key("c")).all(|held| held.failed().is_some()));
    }

    #[test]
    fn signing_out_drops_what_was_held_and_an_ended_session_fails_it() {
        let start = Instant::now();
        let mut hold = Hold::default();
        hold.hold(held("gone", "1"), start);
        hold.hold(held("ended", "2"), start);
        let link = |conversation: &Key| if conversation.id == "gone" { Link::SignedOut } else { Link::Expired };
        assert!(hold.linked(start + secs(1), link).is_empty());
        assert!(!hold.has(&key("gone"), "1"));
        assert_eq!(hold.of(&key("ended")).next().and_then(Held::failed), Some("Slack signed you out."));
        // Signing in again does not send it.
        assert!(hold.linked(start + secs(2), |_| Link::Connected).is_empty());
    }

    #[test]
    fn the_queue_is_bounded() {
        let start = Instant::now();
        let mut hold = Hold::default();
        for id in 0..HELD {
            assert!(hold.hold(held("c", &id.to_string()), start));
        }
        assert!(!hold.hold(held("c", "one more"), start));
    }

    #[test]
    fn a_failed_send_marks_its_conversation_until_resolved() {
        let (a, b) = (key("a"), key("b"));
        let mut flight = Flight::default();
        flight.started(&a, "1");
        assert!(flight.in_flight(&a) && !flight.in_flight(&b));
        assert!(!flight.settle(&a, |_| Outcome::Pending));
        assert!(flight.settle(&a, |_| Outcome::Failed));
        assert!(flight.failed_in(&a) && !flight.in_flight(&a));
        // Retry puts it back on its way; failing again marks it again.
        flight.started(&a, "1");
        assert!(!flight.failed_in(&a));
        flight.settle(&a, |_| Outcome::Failed);
        flight.discarded(&a, "1");
        assert!(!flight.failed_in(&a));
        // A later send that goes clears an older failure.
        flight.started(&a, "2");
        flight.settle(&a, |_| Outcome::Failed);
        flight.started(&a, "3");
        assert!(flight.settle(&a, |id| if id == "3" { Outcome::Sent } else { Outcome::Pending }));
        assert!(!flight.failed_in(&a) && !flight.in_flight(&a));
        flight.started(&b, "4");
        flight.settle(&b, |_| Outcome::Gone);
        assert!(!flight.in_flight(&b));
    }

    #[test]
    fn what_is_followed_is_bounded() {
        let mut flight = Flight::default();
        for id in 0..100 {
            flight.started(&key("a"), &id.to_string());
        }
        assert_eq!(flight.sending[&key("a")].len(), EACH);
        for conversation in 0..(FOLLOWED + 5) {
            flight.started(&key(&conversation.to_string()), "1");
        }
        assert!(flight.sending.len() <= FOLLOWED);
    }
}
