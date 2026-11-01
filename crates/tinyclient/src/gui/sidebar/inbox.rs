//! The Inbox as the rows of one list (`design/polish/04-unified.md` §2): Pinned, then day groups
//! by `updated`, apps folded into one row, and an end line. Which conversations are in it, and
//! their order, are chatd's; the groups, the fold and the held order are the shell's.

use protocol::{Badge, Conversation, InboxFilter, Key};
use std::collections::HashMap;
use std::rc::Rc;

/// App conversations listed under an open Apps row before "Show n more".
pub const APPS_SHOWN: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Day {
    Pinned,
    Today,
    Yesterday,
    /// The five days before yesterday.
    Week,
    Earlier,
}

impl Day {
    pub fn label(self) -> &'static str {
        match self {
            Day::Pinned => "Pinned",
            Day::Today => "Today",
            Day::Yesterday => "Yesterday",
            Day::Week => "This week",
            Day::Earlier => "Earlier",
        }
    }

    /// The group of a row last active `days_ago` calendar days back.
    pub fn of(days_ago: u32) -> Day {
        match days_ago {
            0 => Day::Today,
            1 => Day::Yesterday,
            2..=6 => Day::Week,
            _ => Day::Earlier,
        }
    }
}

/// How many local calendar days before `now` the instant `at` falls; 7 for anything older.
pub fn days_ago(now: i64, at: i64) -> u32 {
    (0..7).find(|days| crate::gui::time::same_day(now - i64::from(*days) * 86_400, at)).unwrap_or(7)
}

/// The one Apps row: the bots and apps of the list, newest first.
#[derive(Clone, PartialEq, Debug)]
pub struct Apps {
    pub members: Vec<Rc<Conversation>>,
    pub open: bool,
}

impl Apps {
    pub fn unread(&self) -> bool {
        self.members.iter().any(|app| app.badge() != Badge::None)
    }

    /// An app that names me raises a mention; nothing else in here counts.
    pub fn mentions(&self) -> u32 {
        self.members.iter().filter(|app| !app.muted).map(|app| app.mentions).sum()
    }

    pub fn newest(&self) -> i64 {
        self.members.first().map_or(0, |app| app.updated)
    }

    /// "Deploybot, Ticketly, Workbot +2".
    pub fn names(&self) -> String {
        let names: Vec<&str> = self.members.iter().take(3).map(|app| app.title.as_str()).collect();
        match self.members.len().saturating_sub(3) {
            0 => names.join(", "),
            more => format!("{} +{more}", names.join(", ")),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    /// A group's header. `first`: the top of the list, 24 tall instead of 28.
    Header { day: Day, first: bool },
    Row { conversation: Rc<Conversation>, day: Day },
    Apps(Rc<Apps>),
    App(Rc<Conversation>),
    /// "Show 3 more" under the open Apps row.
    AppsMore(usize),
    /// "Older conversations are in Discord · Slack".
    End,
}

impl Item {
    pub fn height(&self) -> f32 {
        match self {
            Item::Header { first: true, .. } => 24.,
            Item::Header { .. } | Item::App(_) | Item::AppsMore(_) => 28.,
            Item::Row { .. } | Item::Apps(_) => 48.,
            Item::End => 56.,
        }
    }

    pub fn conversation(&self) -> Option<&Rc<Conversation>> {
        match self {
            Item::Row { conversation, .. } | Item::App(conversation) => Some(conversation),
            _ => None,
        }
    }

    /// What the list keeps its place by when the rows change under it.
    pub fn anchor(&self) -> Option<Key> {
        match self {
            Item::Row { conversation, .. } | Item::App(conversation) => Some(conversation.key.clone()),
            _ => None,
        }
    }
}

/// The Inbox's rows as drawn. `all`: the All chip, with groups; Unread and Mentions are triage
/// lists in chatd's order, with no headers. `apps_open` and `apps_all` are the fold's state.
pub fn items(rows: &[Rc<Conversation>], filter: InboxFilter, now: i64, apps_open: bool, apps_all: bool) -> Vec<Item> {
    let all = matches!(filter, InboxFilter::All | InboxFilter::Directs);
    // An app that names me is a mention like any other, and is listed as one.
    let folded = |c: &Conversation| c.bot && !c.starred && !(c.mentions > 0 && filter == InboxFilter::Mentions);
    let mut apps: Vec<Rc<Conversation>> = rows.iter().filter(|c| folded(c)).cloned().collect();
    apps.sort_by_key(|app| std::cmp::Reverse(app.updated));
    let apps = Apps { members: apps, open: apps_open };
    let shown_apps = !apps.members.is_empty() && (apps.open || apps.unread());
    let day_of = |c: &Conversation| if all && c.starred { Day::Pinned } else { Day::of(days_ago(now, c.updated)) };
    let apps_day = shown_apps.then(|| Day::of(days_ago(now, apps.newest())));

    let mut out = Vec::with_capacity(rows.len() + 8);
    let mut current: Option<Day> = None;
    let push_apps = |out: &mut Vec<Item>, apps: &Apps| {
        let open = apps.open;
        let members = apps.members.clone();
        out.push(Item::Apps(Rc::new(apps.clone())));
        if open {
            let shown = if apps_all { members.len() } else { APPS_SHOWN.min(members.len()) };
            out.extend(members.iter().take(shown).cloned().map(Item::App));
            if members.len() > shown {
                out.push(Item::AppsMore(members.len() - shown));
            }
        }
    };
    let mut apps_left = shown_apps.then_some(apps);
    for row in rows.iter().filter(|c| !folded(c)) {
        let day = day_of(row);
        if all && current != Some(day) {
            // The Apps row closes its own day group, after the people in it; with no group of
            // its own day it gets one, in its place among the others.
            if let (Some(apps), Some(wanted)) = (&apps_left, apps_day) {
                let closing = current == Some(wanted);
                let passed = day > wanted && current.is_none_or(|was| was < wanted);
                if closing || passed {
                    if passed {
                        out.push(Item::Header { day: wanted, first: out.is_empty() });
                    }
                    push_apps(&mut out, apps);
                    apps_left = None;
                }
            }
            out.push(Item::Header { day, first: out.is_empty() });
            current = Some(day);
        }
        out.push(Item::Row { conversation: row.clone(), day });
    }
    if let Some(apps) = apps_left {
        if let Some(wanted) = apps_day.filter(|wanted| all && current != Some(*wanted)) {
            out.push(Item::Header { day: wanted, first: out.is_empty() });
        }
        push_apps(&mut out, &apps);
    }
    if all && !out.is_empty() {
        out.push(Item::End);
    }
    out
}

/// The Inbox while its order is held: the rows stay where they are drawn, each showing what is
/// new about it; rows that newly belong in it wait. Answers the drawn rows and how many have
/// news that would move them, for the "n new" pill.
///
/// `read_missing` is the Unread and Mentions chips. A row that left the fresh query was read,
/// so it stays put with its badge cleared. The chip change is what lets it leave.
pub fn hold(drawn: &[Rc<Conversation>], fresh: &[Rc<Conversation>], read_missing: bool) -> (Vec<Rc<Conversation>>, usize) {
    let by_key: HashMap<&Key, &Rc<Conversation>> = fresh.iter().map(|row| (&row.key, row)).collect();
    let mut waiting = 0;
    let kept = drawn
        .iter()
        .map(|row| match by_key.get(&row.key) {
            Some(now) => {
                if now.updated > row.updated {
                    waiting += 1;
                }
                Rc::clone(now)
            }
            // Gone from the list: it stays until the order is let go.
            None => {
                if read_missing && (row.unread > 0 || row.mentions > 0) {
                    let mut cleared = (**row).clone();
                    cleared.unread = 0;
                    cleared.mentions = 0;
                    Rc::new(cleared)
                } else {
                    Rc::clone(row)
                }
            }
        })
        .collect();
    let drawn_keys: std::collections::HashSet<&Key> = drawn.iter().map(|row| &row.key).collect();
    waiting += fresh.iter().filter(|row| !drawn_keys.contains(&row.key)).count();
    (kept, waiting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ConversationKind;

    const NOW: i64 = 1_760_000_000;

    fn row(id: &str, updated: i64) -> Rc<Conversation> {
        let mut c = crate::gui::sidebar::tests::row(id, ConversationKind::Direct);
        c.updated = updated;
        Rc::new(c)
    }

    fn with(c: &Rc<Conversation>, change: impl FnOnce(&mut Conversation)) -> Rc<Conversation> {
        let mut c = (**c).clone();
        change(&mut c);
        Rc::new(c)
    }

    fn shape(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Header { day, first } => format!("[{}{}]", day.label(), if *first { "^" } else { "" }),
                Item::Row { conversation, .. } => conversation.key.id.clone(),
                Item::Apps(apps) => format!("Apps({})", apps.names()),
                Item::App(app) => format!(".{}", app.key.id),
                Item::AppsMore(more) => format!("+{more}"),
                Item::End => "end".to_owned(),
            })
            .collect()
    }

    #[test]
    fn days_count_back_from_today_and_a_week_ends_in_earlier() {
        assert_eq!([0, 1, 2, 6, 7, 30].map(Day::of), [Day::Today, Day::Yesterday, Day::Week, Day::Week, Day::Earlier, Day::Earlier]);
        assert_eq!(days_ago(NOW, NOW), 0);
        assert_eq!(days_ago(NOW, NOW - 86_400), 1);
        assert_eq!(days_ago(NOW, NOW - 4 * 86_400), 4);
        assert_eq!(days_ago(NOW, NOW - 40 * 86_400), 7);
    }

    #[test]
    fn pinned_first_then_day_groups_with_no_empty_headers_and_an_end_line() {
        let rows = [
            with(&row("star", NOW - 20 * 86_400), |c| c.starred = true),
            row("a", NOW),
            row("b", NOW - 60),
            row("old", NOW - 9 * 86_400),
        ];
        let got = shape(&items(&rows, InboxFilter::All, NOW, false, false));
        assert_eq!(got, ["[Pinned^]", "star", "[Today]", "a", "b", "[Earlier]", "old", "end"]);
        // Unread and Mentions are triage lists: no headers, no end line, chatd's order.
        assert_eq!(shape(&items(&rows, InboxFilter::Unread, NOW, false, false)), ["star", "a", "b", "old"]);
    }

    #[test]
    fn apps_fold_into_one_row_shown_only_while_unread_after_their_day_groups_people() {
        let app = |id: &str, updated, unread| with(&row(id, updated), |c| (c.bot, c.unread) = (true, unread));
        let rows = [
            row("ines", NOW),
            app("deploybot", NOW - 30, 2),
            app("ticketly", NOW - 40, 0),
            row("mara", NOW - 86_400),
        ];
        let got = shape(&items(&rows, InboxFilter::All, NOW, false, false));
        assert_eq!(got, ["[Today^]", "ines", "Apps(deploybot, ticketly)", "[Yesterday]", "mara", "end"]);

        // All read: no Apps row at all, unless the user opened it.
        let read: Vec<_> = rows.iter().map(|c| with(c, |c| c.unread = 0)).collect();
        assert_eq!(shape(&items(&read, InboxFilter::All, NOW, false, false)), ["[Today^]", "ines", "[Yesterday]", "mara", "end"]);
        let open = shape(&items(&read, InboxFilter::All, NOW, true, false));
        assert_eq!(open, ["[Today^]", "ines", "Apps(deploybot, ticketly)", ".deploybot", ".ticketly", "[Yesterday]", "mara", "end"]);

        // An app never raises a number; one that names me does, and Mentions lists it as a row.
        let named = with(&rows[2], |c| (c.mentions, c.unread) = (1, 1));
        let apps = Apps { members: vec![rows[1].clone(), named.clone()], open: false };
        assert_eq!((apps.unread(), apps.mentions()), (true, 1));
        assert_eq!(shape(&items(&[named], InboxFilter::Mentions, NOW, false, false)), ["ticketly"]);
    }

    #[test]
    fn apps_older_than_every_row_get_their_own_group_and_open_shows_eight_then_more() {
        let apps: Vec<_> = (0..11).map(|n| with(&row(&format!("app{n}"), NOW - 3 * 86_400 - n), |c| (c.bot, c.unread) = (true, 1))).collect();
        let mut rows = vec![row("ines", NOW)];
        rows.extend(apps);
        let got = shape(&items(&rows, InboxFilter::All, NOW, true, false));
        assert_eq!(got[..4], ["[Today^]", "ines", "[This week]", "Apps(app0, app1, app2 +8)"]);
        assert_eq!(got[got.len() - 2..], ["+3", "end"]);
        assert_eq!(got.len(), 4 + 8 + 2);
        assert_eq!(items(&rows, InboxFilter::All, NOW, true, true).len(), 4 + 11 + 1);
    }

    #[test]
    fn a_held_list_changes_rows_in_place_and_counts_what_waits() {
        let drawn = vec![row("a", 10), row("b", 9), row("c", 8)];
        // b spoke, d is new, c left the list.
        let fresh = vec![row("b", 20), row("d", 15), row("a", 10)];
        let (kept, waiting) = hold(&drawn, &fresh, false);
        let order: Vec<(&str, i64)> = kept.iter().map(|c| (c.key.id.as_str(), c.updated)).collect();
        assert_eq!(order, [("a", 10), ("b", 20), ("c", 8)]);
        assert_eq!(waiting, 2);
        assert_eq!(hold(&fresh, &fresh, false).1, 0);
    }

    #[test]
    fn a_row_that_leaves_the_unread_chip_keeps_its_place_and_loses_its_badge() {
        let drawn = vec![with(&row("a", 10), |c| c.unread = 1)];
        let fresh = vec![row("b", 20)];
        let (kept, _) = hold(&drawn, &fresh, true);
        assert_eq!(kept[0].key.id, "a");
        assert_eq!((kept[0].unread, kept[0].mentions), (0, 0));
        let (kept, _) = hold(&drawn, &fresh, false);
        assert_eq!(kept[0].unread, 1);
    }
}
