//! A Space's tree as the flat list of rows on show. Rebuilt on collapse; one row height.

use protocol::{Badge, Conversation, ConversationKind, Group, Key, Occupant};
use std::collections::HashMap;
use std::rc::Rc;

/// Direct conversations a network home shows before `Show n more`.
pub const DIRECT_SHOWN: usize = 6;
/// People listed under a voice channel before `+n more`.
pub const OCCUPANTS_SHOWN: usize = 6;
/// Threads listed under a channel before `n more threads`.
pub const THREADS_SHOWN: usize = 5;

/// What a collapsed Group or a Space says about what is inside it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Rollup {
    /// The sum of the numbers inside.
    pub attention: u32,
    /// Every one of those numbers is a mention in something muted.
    pub quiet: bool,
    /// Something unmuted inside is unread with no number.
    pub unread: bool,
}

impl Rollup {
    pub fn of<'a>(conversations: impl IntoIterator<Item = &'a Conversation>) -> Rollup {
        let mut rollup = Rollup { quiet: true, ..Rollup::default() };
        for conversation in conversations {
            match conversation.badge() {
                Badge::Count(count) => {
                    rollup.attention += count;
                    rollup.quiet &= conversation.muted;
                }
                Badge::Dot => rollup.unread = true,
                Badge::None => {}
            }
        }
        rollup.quiet &= rollup.attention > 0;
        rollup
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Row {
    /// A section label that does not collapse: `Direct messages`.
    Label(&'static str),
    /// A Space in a network home. `index` is into the shell's list of Spaces.
    Space { index: usize, open: bool },
    Group { space: Key, id: String, name: String, collapsed: bool, rollup: Rollup, depth: u8 },
    Conversation { conversation: Rc<Conversation>, depth: u8 },
    Occupant { occupant: Occupant, depth: u8 },
    /// `+4 more` under a voice channel, `3 more threads` under a channel.
    More { words: String, depth: u8 },
    /// `Show 76 more` under the direct conversations of a network home.
    ShowDirect { more: usize },
    /// A Space's rows are still on their way.
    Loading { depth: u8 },
}

impl Row {
    pub fn conversation(&self) -> Option<&Rc<Conversation>> {
        match self {
            Row::Conversation { conversation, .. } => Some(conversation),
            _ => None,
        }
    }
}

/// One Space's rows: what has no Group first, then each Group under its header, all in the
/// network's own order. A thread sits under the conversation it hangs off, a voice channel's
/// occupants under it. A collapsed Group keeps only its header, with what is inside rolled up,
/// and the open conversation's row so the place I am in never disappears.
pub fn flatten(space: &Key, groups: &[Group], conversations: &[Rc<Conversation>], open: Option<&Key>, depth: u8) -> Vec<Row> {
    let mut rows = Vec::with_capacity(conversations.len() + groups.len());
    let known = |id: &Option<String>| id.as_ref().is_some_and(|id| groups.iter().any(|g| &g.id == id));
    // A thread whose parent is in this tree is not a row of its own Group: it goes under it.
    let mut threads: HashMap<&str, Vec<&Rc<Conversation>>> = HashMap::new();
    let nested = |c: &Conversation| {
        c.kind == ConversationKind::Thread
            && c.parent.as_ref().is_some_and(|parent| conversations.iter().any(|other| &other.key.id == parent))
    };
    for thread in conversations.iter().filter(|c| nested(c)) {
        threads.entry(thread.parent.as_deref().unwrap_or_default()).or_default().push(thread);
    }
    let is_open = |c: &Conversation| open == Some(&c.key);
    let emit = |rows: &mut Vec<Row>, members: &[&Rc<Conversation>], depth: u8, only_open: bool| {
        for conversation in members {
            let under = threads.get(conversation.key.id.as_str()).map(Vec::as_slice).unwrap_or_default();
            if only_open {
                let shown = std::iter::once(*conversation).chain(under.iter().copied()).find(|c| is_open(c));
                rows.extend(shown.map(|c| Row::Conversation { conversation: Rc::clone(c), depth }));
                continue;
            }
            rows.push(Row::Conversation { conversation: Rc::clone(conversation), depth });
            // The first few, and the one I am in wherever it falls.
            let mut hidden = 0;
            for (ix, thread) in under.iter().enumerate() {
                if ix < THREADS_SHOWN || is_open(thread) {
                    rows.push(Row::Conversation { conversation: Rc::clone(thread), depth: depth + 1 });
                } else {
                    hidden += 1;
                }
            }
            if hidden > 0 {
                let words = if hidden == 1 { "1 more thread".to_owned() } else { format!("{hidden} more threads") };
                rows.push(Row::More { words, depth: depth + 1 });
            }
            if let Some(inside) = conversation.occupants.as_ref().filter(|_| conversation.kind == ConversationKind::Voice) {
                let listed = inside.people.iter().take(OCCUPANTS_SHOWN);
                rows.extend(listed.map(|occupant| Row::Occupant { occupant: occupant.clone(), depth: depth + 1 }));
                let more = (inside.total as usize).saturating_sub(OCCUPANTS_SHOWN.min(inside.people.len()));
                if more > 0 {
                    rows.push(Row::More { words: format!("+{more} more"), depth: depth + 1 });
                }
            }
        }
    };

    let loose: Vec<&Rc<Conversation>> = conversations.iter().filter(|c| !known(&c.group) && !nested(c)).collect();
    emit(&mut rows, &loose, depth, false);
    let mut ordered: Vec<&Group> = groups.iter().collect();
    ordered.sort_by_key(|group| group.position);
    for group in ordered {
        let members: Vec<&Rc<Conversation>> =
            conversations.iter().filter(|c| c.group.as_deref() == Some(&group.id) && !nested(c)).collect();
        if members.is_empty() {
            continue;
        }
        let rollup = if group.collapsed {
            let inside = members.iter().flat_map(|c| {
                let under = threads.get(c.key.id.as_str()).map(Vec::as_slice).unwrap_or_default();
                std::iter::once(&***c).chain(under.iter().map(|thread| &***thread))
            });
            Rollup::of(inside)
        } else {
            Rollup::default()
        };
        rows.push(Row::Group {
            space: space.clone(),
            id: group.id.clone(),
            name: group.name.clone(),
            collapsed: group.collapsed,
            rollup,
            depth,
        });
        emit(&mut rows, &members, depth, group.collapsed);
    }
    rows
}

/// The rows after `from` (or before it, going up) that a keyboard walk may land on: the next
/// conversation, or the next unread one.
pub fn walk(rows: &[Row], from: Option<usize>, down: bool, unread_only: bool) -> Option<usize> {
    let wanted = |row: &Row| {
        row.conversation().is_some_and(|c| !unread_only || c.badge() != Badge::None)
    };
    if down {
        let start = from.map_or(0, |from| from + 1);
        (start..rows.len()).find(|ix| wanted(&rows[*ix]))
    } else {
        let end = from.unwrap_or(rows.len());
        (0..end).rev().find(|ix| wanted(&rows[*ix]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CallSupport, Network};

    fn key(id: &str) -> Key {
        Key { network: Network::Discord, account: "me".into(), id: id.into() }
    }

    fn channel(id: &str, group: Option<&str>, kind: ConversationKind) -> Conversation {
        Conversation {
            root: None,
            space_title: String::new(),
            topic: String::new(),
            parent: None,
            private: false,
            read_only: false,
            draft: String::new(),
            group_title: String::new(),
            occupants: None,
            key: key(id),
            kind,
            title: id.into(),
            space: Some("s".into()),
            group: group.map(str::to_owned),
            position: 0,
            avatar: None,
            peer: None,
            unread: 0,
            mentions: 0,
            muted: false,
            starred: false,
            preview: String::new(),
            updated: 0,
            last_read: None,
            call: CallSupport::None,
        }
    }

    fn group(id: &str, position: i32, collapsed: bool) -> Group {
        Group { count: 0, kind: Default::default(), id: id.into(), name: id.into(), position, collapsed }
    }

    /// One word per row, so a whole tree reads as a line.
    fn shape(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Group { id, collapsed, rollup, .. } => {
                    let mark = if *collapsed { ">" } else { "v" };
                    match (rollup.attention, rollup.unread) {
                        (0, false) => format!("{mark}{id}"),
                        (0, true) => format!("{mark}{id}*"),
                        (count, _) => format!("{mark}{id}({count}{})", if rollup.quiet { "q" } else { "" }),
                    }
                }
                Row::Conversation { conversation, depth } => {
                    format!("{}{}", ".".repeat(*depth as usize), conversation.key.id)
                }
                Row::Occupant { occupant, depth } => format!("{}@{}", ".".repeat(*depth as usize), occupant.name),
                Row::More { words, .. } => words.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn rows(groups: &[Group], conversations: Vec<Conversation>, open: Option<&str>) -> Vec<String> {
        let conversations: Vec<Rc<Conversation>> = conversations.into_iter().map(Rc::new).collect();
        let open = open.map(key);
        shape(&flatten(&key("s"), groups, &conversations, open.as_ref(), 0))
    }

    #[test]
    fn groups_come_in_the_networks_order_with_loose_rows_first_and_threads_under_their_parent() {
        use ConversationKind::{Channel, Thread};
        // The thread comes last in the network's order and still sits under the channel it names.
        let mut thread = channel("general-thread", Some("text"), Thread);
        thread.parent = Some("general".into());
        let tree = rows(
            &[group("voice", 2, false), group("text", 1, false), group("empty", 0, false)],
            vec![
                channel("rules", None, Channel),
                channel("lounge", Some("voice"), ConversationKind::Voice),
                channel("general", Some("text"), Channel),
                channel("help", Some("text"), Channel),
                // Its group is not in the list: it is shown rather than lost.
                channel("orphan", Some("gone"), Channel),
                thread,
            ],
            None,
        );
        assert_eq!(tree, ["rules", "orphan", "vtext", "general", ".general-thread", "help", "vvoice", "lounge"]);
    }

    #[test]
    fn a_collapsed_group_keeps_the_open_row_and_says_what_is_inside() {
        use ConversationKind::Channel;
        let mut mentioned = channel("design", Some("text"), Channel);
        (mentioned.unread, mentioned.mentions) = (4, 2);
        let mut unread = channel("general", Some("text"), Channel);
        unread.unread = 1;
        let quiet = |muted_mentions: u32| {
            let mut row = channel("bots", Some("quiet"), Channel);
            (row.muted, row.unread, row.mentions) = (true, 9, muted_mentions);
            row
        };
        let groups = [group("text", 0, true), group("quiet", 1, true)];
        let all = |muted_mentions| vec![mentioned.clone(), unread.clone(), channel("help", Some("text"), Channel), quiet(muted_mentions)];

        // A muted channel with no mention raises nothing.
        assert_eq!(rows(&groups, all(0), None), [">text(2)", ">quiet"]);
        // A mention in a muted channel is a number, and a quiet one when nothing else is.
        assert_eq!(rows(&groups, all(3), Some("help")), [">text(2)", "help", ">quiet(3q)"]);

        let dot_only = vec![unread.clone(), channel("help", Some("text"), Channel)];
        assert_eq!(rows(&groups[..1], dot_only, None), [">text*"]);
    }

    #[test]
    fn a_channel_lists_five_threads_then_a_count_and_always_the_open_one() {
        use ConversationKind::{Channel, Thread};
        let mut all = vec![channel("general", None, Channel)];
        for n in 0..8 {
            let mut thread = channel(&format!("t{n}"), None, Thread);
            thread.parent = Some("general".into());
            all.push(thread);
        }
        assert_eq!(rows(&[], all.clone(), None), ["general", ".t0", ".t1", ".t2", ".t3", ".t4", "3 more threads"]);
        assert_eq!(rows(&[], all.clone(), Some("t7")), ["general", ".t0", ".t1", ".t2", ".t3", ".t4", ".t7", "2 more threads"]);
        // In a collapsed Group the open thread still shows, and the threads count in the roll-up.
        for row in &mut all {
            row.group = Some("text".into());
            row.unread = u32::from(row.key.id == "t6");
        }
        assert_eq!(rows(&[group("text", 0, true)], all, Some("t7")), [">text*", "t7"]);
    }

    #[test]
    fn a_voice_channel_lists_six_people_then_a_count() {
        let mut lounge = channel("lounge", None, ConversationKind::Voice);
        let people = (0..9).map(|n| Occupant { person: key(&n.to_string()), name: n.to_string(), muted: false, deafened: false, streaming: false });
        // The row names at most 25; the total says how many there are.
        lounge.occupants = Some(protocol::Occupants { people: people.collect(), total: 40 });
        assert_eq!(rows(&[], vec![lounge], None), ["lounge", ".@0", ".@1", ".@2", ".@3", ".@4", ".@5", "+34 more"]);
        assert_eq!(rows(&[], vec![channel("empty", None, ConversationKind::Voice)], None), ["empty"]);
    }

    #[test]
    fn the_unread_walk_skips_headers_and_read_rows_in_both_directions() {
        use ConversationKind::Channel;
        let unread = |id: &str| {
            let mut row = channel(id, Some("text"), Channel);
            row.unread = 1;
            row
        };
        let conversations: Vec<Rc<Conversation>> =
            [channel("a", Some("text"), Channel), unread("b"), channel("c", Some("text"), Channel), unread("d")]
                .into_iter()
                .map(Rc::new)
                .collect();
        let rows = flatten(&key("s"), &[group("text", 0, false)], &conversations, None, 0);
        // Row 0 is the header.
        assert_eq!(walk(&rows, None, true, false), Some(1));
        assert_eq!(walk(&rows, None, true, true), Some(2));
        assert_eq!(walk(&rows, Some(2), true, true), Some(4));
        assert_eq!(walk(&rows, Some(4), true, true), None);
        assert_eq!(walk(&rows, Some(4), false, true), Some(2));
        assert_eq!(walk(&rows, Some(1), false, false), None);
    }
}
