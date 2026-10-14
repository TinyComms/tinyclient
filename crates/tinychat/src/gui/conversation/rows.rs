//! What the list holds: messages, and the lines between them. Which message starts a group and
//! where the dividers go is decided here, apart from any drawing.

use protocol::{Message, MessageKind};
use std::ops::Range;
use std::rc::Rc;

/// Messages from one sender this close together share a header.
pub const GROUP_SECONDS: i64 = 5 * 60;

#[derive(Clone, Debug)]
pub enum Item {
    /// History is exhausted: who or what this conversation is.
    Start,
    /// Older messages exist and are being asked for.
    Older,
    /// The message a thread hangs off, at the top of the thread pane.
    Root(Rc<Message>),
    /// "4 replies", under the root.
    Replies(u32),
    Day(i64),
    New,
    Message { message: Rc<Message>, head: bool },
    System(Rc<Message>),
}

impl PartialEq for Item {
    fn eq(&self, other: &Item) -> bool {
        let same = |a: &Rc<Message>, b: &Rc<Message>| Rc::ptr_eq(a, b) || a == b;
        match (self, other) {
            (Item::Start, Item::Start) | (Item::Older, Item::Older) | (Item::New, Item::New) => true,
            (Item::Replies(a), Item::Replies(b)) => a == b,
            (Item::Day(a), Item::Day(b)) => a == b,
            (Item::Root(a), Item::Root(b)) | (Item::System(a), Item::System(b)) => same(a, b),
            (Item::Message { message: a, head: x }, Item::Message { message: b, head: y }) => {
                x == y && same(a, b)
            }
            _ => false,
        }
    }
}

impl Item {
    pub fn message(&self) -> Option<&Rc<Message>> {
        match self {
            Item::Root(message) | Item::System(message) | Item::Message { message, .. } => Some(message),
            _ => None,
        }
    }

    /// The same thing before and after a change, whatever it now says.
    fn same_place(&self, other: &Item) -> bool {
        match (self.message(), other.message()) {
            (Some(a), Some(b)) => a.id == b.id,
            (None, None) => match (self, other) {
                (Item::Day(a), Item::Day(b)) => a == b,
                _ => std::mem::discriminant(self) == std::mem::discriminant(other),
            },
            _ => false,
        }
    }
}

pub struct Build<'a> {
    /// Where the user had read to when the conversation was opened.
    pub last_read: Option<&'a str>,
    pub older: bool,
    /// The thread pane: its root on top, and no start-of-conversation header.
    pub root: Option<(Rc<Message>, u32)>,
    pub same_day: fn(i64, i64) -> bool,
}

/// Whether `message` carries on the group `before` is in.
fn continues(before: &Message, message: &Message, same_day: fn(i64, i64) -> bool) -> bool {
    before.kind == MessageKind::Normal
        && message.kind == MessageKind::Normal
        && message.reply_to.is_none()
        // The same bot user posting as a different app is a different author.
        && before.sender.id == message.sender.id
        && before.sender.name == message.sender.name
        && (0..=GROUP_SECONDS).contains(&(message.sent - before.sent))
        && same_day(before.sent, message.sent)
}

pub fn build(messages: &[Rc<Message>], build: &Build) -> Vec<Item> {
    let mut items = Vec::with_capacity(messages.len() + 8);
    match &build.root {
        Some((root, replies)) => {
            items.push(Item::Root(root.clone()));
            items.push(Item::Replies(*replies));
        }
        None if build.older => items.push(Item::Older),
        None => items.push(Item::Start),
    }
    // The divider goes above the first message after the marker, and only if someone else
    // wrote something there.
    let first_new = build
        .last_read
        .and_then(|read| messages.iter().position(|message| message.id == read))
        .map(|read| read + 1)
        .filter(|first| messages[*first..].iter().any(|message| !message.outgoing));

    for (ix, message) in messages.iter().enumerate() {
        let before = ix.checked_sub(1).map(|ix| &messages[ix]);
        let new = first_new == Some(ix);
        let new_day = match before {
            Some(before) => !(build.same_day)(before.sent, message.sent),
            // A thread's replies start under its root, which carries the day.
            None => build.root.as_ref().is_none_or(|(root, _)| !(build.same_day)(root.sent, message.sent)),
        };
        if new {
            items.push(Item::New);
        }
        if new_day {
            items.push(Item::Day(message.sent));
        }
        if message.kind == MessageKind::System {
            items.push(Item::System(message.clone()));
            continue;
        }
        let head = new || new_day || before.is_none_or(|before| !continues(before, message, build.same_day));
        items.push(Item::Message { message: message.clone(), head });
    }
    items
}

/// The smallest replacement that turns `old` into `new`: the rows before and after it are the
/// same, so the list keeps their measured heights.
pub fn changed(old: &[Item], new: &[Item]) -> Option<(Range<usize>, usize)> {
    let front = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if front == old.len() && front == new.len() {
        return None;
    }
    let back = old[front..]
        .iter()
        .rev()
        .zip(new[front..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    Some((front..old.len() - back, new.len() - front - back))
}

/// Where the top of the viewport is after a change, so what the user was reading does not move:
/// the same row at the same offset, or the next row down that survived.
pub fn anchor(old: &[Item], new: &[Item], top: usize, offset: f32) -> Option<(usize, f32)> {
    old.get(top..)?.iter().enumerate().find_map(|(skipped, item)| {
        let ix = new.iter().position(|candidate| candidate.same_place(item))?;
        Some((ix, if skipped == 0 { offset } else { 0. }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ReplyTo;

    const DAY: i64 = 86_400;

    fn utc_day(a: i64, b: i64) -> bool {
        a.div_euclid(DAY) == b.div_euclid(DAY)
    }

    fn said(id: &str, who: &str, at: i64) -> Rc<Message> {
        let mut message = Message::plain(id, who, "hello", at, who == "You");
        message.sender.id = who.to_lowercase();
        Rc::new(message)
    }

    fn with(message: Rc<Message>, change: impl FnOnce(&mut Message)) -> Rc<Message> {
        let mut message = (*message).clone();
        change(&mut message);
        Rc::new(message)
    }

    fn lay(messages: &[Rc<Message>], last_read: Option<&str>) -> Vec<Item> {
        build(messages, &Build { last_read, older: false, root: None, same_day: utc_day })
    }

    /// One letter per row: `S`tart, `D`ay, `N`ew, `H`ead, `c`ontinuation, `s`ystem.
    fn shape(items: &[Item]) -> String {
        items
            .iter()
            .map(|item| match item {
                Item::Start => 'S',
                Item::Older => 'O',
                Item::Root(_) => 'R',
                Item::Replies(_) => '#',
                Item::Day(_) => 'D',
                Item::New => 'N',
                Item::Message { head: true, .. } => 'H',
                Item::Message { head: false, .. } => 'c',
                Item::System(_) => 's',
            })
            .collect()
    }

    #[test]
    fn a_group_breaks_on_sender_time_day_reply_and_system_lines() {
        let t = 10 * DAY + 3_600;
        let messages = [
            said("1", "Mara", t),
            said("2", "Mara", t + 60),
            said("3", "Mara", t + 60 + GROUP_SECONDS),
            said("4", "Mara", t + 61 + 2 * GROUP_SECONDS),
            said("5", "Dev", t + 62 + 2 * GROUP_SECONDS),
            with(said("6", "Dev", t + 63 + 2 * GROUP_SECONDS), |m| {
                m.reply_to = Some(ReplyTo { deleted: false, person: None, id: "1".into(), sender: "Mara".into(), snippet: "hello".into() });
            }),
            with(said("7", "Dev", t + 64 + 2 * GROUP_SECONDS), |m| m.kind = MessageKind::System),
            said("8", "Dev", t + 65 + 2 * GROUP_SECONDS),
            said("9", "Dev", t + DAY),
            said("10", "Dev", t + DAY + 1),
        ];
        assert_eq!(shape(&lay(&messages, None)), "SDHccHHHsHDHc");
    }

    #[test]
    fn the_same_bot_under_another_app_name_starts_a_group() {
        let t = 10 * DAY;
        let ci = with(said("1", "CI", t), |m| m.sender.id = "B1".into());
        let deploys = with(said("2", "Deploys", t + 1), |m| m.sender.id = "B1".into());
        assert_eq!(shape(&lay(&[ci, deploys], None)), "SDHH");
    }

    #[test]
    fn the_new_divider_sits_above_the_first_unread_and_above_a_day_divider() {
        let t = 10 * DAY;
        let messages = [
            said("1", "Mara", t),
            said("2", "Mara", t + 1),
            said("3", "Mara", t + 2),
            said("4", "Mara", t + DAY),
        ];
        assert_eq!(shape(&lay(&messages, Some("1"))), "SDHNHcDH");
        assert_eq!(shape(&lay(&messages, Some("3"))), "SDHccNDH");
        // Everything read, or only my own messages after the marker: no divider.
        assert_eq!(shape(&lay(&messages, Some("4"))), "SDHccDH");
        let mine = [said("1", "Mara", t), said("2", "You", t + 1)];
        assert_eq!(shape(&lay(&mine, Some("1"))), "SDHH");
        assert_eq!(shape(&lay(&messages, Some("gone"))), "SDHccDH");
    }

    #[test]
    fn a_thread_starts_with_its_root_and_a_count() {
        let t = 10 * DAY;
        let root = said("r", "Mara", t);
        let replies = [said("1", "Dev", t + 5), said("2", "Dev", t + 6)];
        let items = build(&replies, &Build {
            last_read: None,
            older: false,
            root: Some((root, 2)),
            same_day: utc_day,
        });
        assert_eq!(shape(&items), "R#Hc");
    }

    #[test]
    fn only_the_rows_that_differ_are_replaced() {
        let t = 10 * DAY + 3_600;
        let messages: Vec<_> = (0..6).map(|i| said(&i.to_string(), "Mara", t + i)).collect();
        let old = lay(&messages, None);
        assert_eq!(changed(&old, &old), None);

        // A reaction on message 3: one row.
        let mut reacted = messages.clone();
        reacted[3] = with(reacted[3].clone(), |m| m.edited = true);
        assert_eq!(changed(&old, &lay(&reacted, None)), Some((5..6, 1)));

        // A new message at the end: nothing replaced, one row added.
        let mut longer = messages.clone();
        longer.push(said("6", "Mara", t + 6));
        assert_eq!(changed(&old, &lay(&longer, None)), Some((8..8, 1)));

        // Message 2 deleted: its row goes and nothing else is touched.
        let mut shorter = messages.clone();
        shorter.remove(2);
        assert_eq!(changed(&old, &lay(&shorter, None)), Some((4..5, 0)));

        // Older history arriving: the day line now belongs to the first of the page.
        let mut earlier: Vec<_> = (0..3).map(|i| said(&format!("e{i}"), "Dev", t - 100 + i)).collect();
        earlier.extend(messages.iter().cloned());
        let new = lay(&earlier, None);
        assert_eq!(shape(&new), "SDHccHccccc");
        assert_eq!(changed(&old, &new), Some((1..2, 4)));
    }

    #[test]
    fn the_row_at_the_top_stays_at_the_top_when_rows_arrive_or_leave_above_it() {
        let t = 10 * DAY + 3_600;
        let messages: Vec<_> = (0..6).map(|i| said(&i.to_string(), "Mara", t + i)).collect();
        let old = lay(&messages, None);
        // Reading message 3 (row 5), 12 px into it.
        let mut earlier: Vec<_> = (0..40).map(|i| said(&format!("e{i}"), "Dev", t - 100 + i)).collect();
        earlier.extend(messages.iter().cloned());
        assert_eq!(anchor(&old, &lay(&earlier, None), 5, 12.), Some((45, 12.)));

        // The row being read is deleted: the next one takes its place at its top.
        let mut shorter = messages.clone();
        shorter.remove(3);
        assert_eq!(anchor(&old, &lay(&shorter, None), 5, 12.), Some((5, 0.)));

        // An edit above changes nothing about where the viewport is.
        let mut edited = messages.clone();
        edited[1] = with(edited[1].clone(), |m| m.text = "longer\nnow".into());
        assert_eq!(anchor(&old, &lay(&edited, None), 5, 12.), Some((5, 12.)));
    }
}
