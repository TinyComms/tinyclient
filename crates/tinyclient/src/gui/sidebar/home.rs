//! The network homes that are lists of faces (`design/polish/02-discord-slack.md` §3,
//! `03-telegram-teams.md` §2, §5): Discord's Friends, requests, In voice, DMs and servers in
//! folders; Telegram's one chat list under folder tabs, with its Archive; Teams' Pinned, Chats,
//! Meetings and teams. Slack's home is its workspace's tree (`tree.rs`). Rows are 48 and 28
//! mixed, so the list is a GPUI `list` whose heights are known from each item's kind.

use protocol::{Badge, Conversation, Folder, Key, Network, Space, Special};
use std::collections::HashSet;
use std::rc::Rc;

/// Discord DMs before "Show n more" (product rulings: 10).
pub const DMS_SHOWN: usize = 10;
/// In-voice rows before "n more".
pub const VOICE_SHOWN: usize = 3;
/// Teams chats shown at least, before "Show n more".
pub const CHATS_SHOWN: usize = 8;
/// A Teams chat active this recently is shown without asking.
pub const CHATS_ACTIVE: i64 = 7 * 86_400;
/// A meeting chat quiet this long moves into the Meetings fold.
pub const MEETING_QUIET: i64 = 14 * 86_400;

/// A list the home drills into, replacing itself until `‹`.
#[derive(Clone, PartialEq, Debug)]
pub enum Drill {
    /// A Slack workspace's followed threads.
    Threads(Key),
    /// Telegram's archive.
    Archive,
    /// Discord's message requests.
    Requests,
    /// A forum's topics.
    Topics(Rc<Conversation>),
}

/// What an action row does.
#[derive(Clone, PartialEq, Debug)]
pub enum Act {
    Friends,
    Requests,
    MoreDms,
    MoreVoice,
    Archive,
    Meetings,
    MoreChats,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    Header { label: &'static str, first: bool },
    Chat(Rc<Conversation>),
    /// A voice channel or DM call with friends in it.
    Voice(Rc<Conversation>),
    /// A 28 row that does something: `words` and the faint words or number after them.
    Action { act: Act, icon: &'static str, words: String, after: String, dot: Option<bool> },
    /// A server or team. `child`: inside a folder, on its spine.
    Space { index: usize, child: Option<u32>, open: Option<bool> },
    Folder { id: String, name: String, color: Option<u32>, open: bool, lit: bool, mention: bool },
    /// A team's channel under its row.
    Channel(Rc<Conversation>),
}

impl Item {
    pub fn height(&self) -> f32 {
        match self {
            Item::Header { first: true, .. } => 24.,
            Item::Chat(_) | Item::Voice(_) => 48.,
            _ => 28.,
        }
    }

    pub fn conversation(&self) -> Option<&Rc<Conversation>> {
        match self {
            Item::Chat(c) | Item::Voice(c) | Item::Channel(c) => Some(c),
            _ => None,
        }
    }

    pub fn anchor(&self) -> Option<String> {
        match self {
            Item::Chat(c) | Item::Voice(c) | Item::Channel(c) => Some(format!("c/{}", c.key.id)),
            Item::Folder { id, .. } => Some(format!("f/{id}")),
            Item::Header { label, .. } => Some(format!("h/{label}")),
            _ => None,
        }
    }
}

/// Everything a home shows, read from chatd.
#[derive(Default)]
pub struct Home {
    pub network: Option<Network>,
    /// The network's own list: DMs, chats, in its order.
    pub chats: Vec<Rc<Conversation>>,
    /// Discord's message requests, counted.
    pub requests: u32,
    pub in_voice: Vec<Rc<Conversation>>,
    pub folders: Vec<Folder>,
    /// Discord friends online, and friend requests waiting (`Around`).
    pub online: usize,
    pub friend_requests: u32,
    /// Telegram's archive: its newest three and how many.
    pub archive: Vec<Rc<Conversation>>,
    pub archive_total: u32,
    /// The rows of the list drilled into.
    pub drilled: Vec<Rc<Conversation>>,
    /// The Telegram folder chosen, by id.
    pub folder: Option<String>,
    pub loaded: bool,
    pub epoch: u64,
    // The user's folds, for this session.
    pub dms_all: bool,
    pub voice_all: bool,
    pub chats_all: bool,
    pub meetings_open: bool,
    pub opened: HashSet<String>,
}

impl Home {
    pub fn for_network(network: Network) -> Home {
        Home { network: Some(network), ..Home::default() }
    }
}

fn needs_me(c: &Conversation) -> bool {
    matches!(c.badge(), Badge::Count(_))
}

fn header(out: &mut Vec<Item>, label: &'static str) {
    out.push(Item::Header { label, first: out.is_empty() });
}

/// The Discord home, top to bottom: Friends, Message requests, In voice, DMs, servers by the
/// user's order with folders. `open_space`: the Space of the open conversation. `unread`: the
/// Unread segment, which keeps only what has news.
pub fn discord(home: &Home, spaces: &[Space], open_space: Option<&str>, unread: bool) -> Vec<Item> {
    let mut out = Vec::new();
    if !unread || home.friend_requests > 0 {
        let after = if home.online > 0 { format!("{} online", home.online) } else { String::new() };
        out.push(Item::Action { act: Act::Friends, icon: "icons/users.svg", words: "Friends".into(), after, dot: None });
    }
    if home.requests > 0 && !unread {
        out.push(Item::Action { act: Act::Requests, icon: "icons/mail.svg", words: "Message requests".into(), after: home.requests.to_string(), dot: None });
    }
    if !home.in_voice.is_empty() && !unread {
        header(&mut out, "In voice");
        let shown = if home.voice_all { home.in_voice.len() } else { VOICE_SHOWN.min(home.in_voice.len()) };
        out.extend(home.in_voice.iter().take(shown).cloned().map(Item::Voice));
        if home.in_voice.len() > shown {
            out.push(Item::Action { act: Act::MoreVoice, icon: "icons/chevron-down.svg", words: format!("{} more", home.in_voice.len() - shown), after: String::new(), dot: None });
        }
    }
    let dms = shown_dms(&home.chats, home.dms_all, unread);
    if !dms.0.is_empty() {
        header(&mut out, "Direct messages");
        out.extend(dms.0.into_iter().map(Item::Chat));
        if dms.1 > 0 {
            out.push(Item::Action { act: Act::MoreDms, icon: "icons/chevron-down.svg", words: format!("Show {} more", dms.1), after: String::new(), dot: None });
        }
    }
    let servers = servers(spaces, &home.folders, &home.opened, open_space, unread);
    if !servers.is_empty() {
        header(&mut out, "Servers");
        out.extend(servers);
    }
    out
}

/// Every DM with a number, then the newest up to ten in all, in Discord's order. Answers the
/// rows and how many more there are.
pub fn shown_dms(chats: &[Rc<Conversation>], all: bool, unread: bool) -> (Vec<Rc<Conversation>>, usize) {
    if unread {
        return (chats.iter().filter(|c| c.badge() != Badge::None).cloned().collect(), 0);
    }
    if all {
        return (chats.to_vec(), 0);
    }
    let numbered = chats.iter().filter(|c| needs_me(c)).count();
    let mut room = DMS_SHOWN.saturating_sub(numbered);
    let shown: Vec<Rc<Conversation>> = chats
        .iter()
        .filter(|c| {
            if needs_me(c) {
                return true;
            }
            let take = room > 0;
            room = room.saturating_sub(1);
            take
        })
        .cloned()
        .collect();
    let more = chats.len() - shown.len();
    (shown, more)
}

/// Servers in the user's order, folders as groups. A folder starts collapsed and still lists
/// the open server and every server holding a number (rule 3).
pub fn servers(spaces: &[Space], folders: &[Folder], opened: &HashSet<String>, open_space: Option<&str>, unread: bool) -> Vec<Item> {
    let mut out = Vec::new();
    let mut done: HashSet<&str> = HashSet::new();
    let news = |space: &Space| space.attention > 0 || space.unread;
    let wanted = |space: &Space| !unread || news(space) || open_space == Some(space.key.id.as_str());
    for (index, space) in spaces.iter().enumerate() {
        if done.contains(space.key.id.as_str()) {
            continue;
        }
        let folder = space.folder.as_ref().and_then(|id| folders.iter().find(|f| f.key.id == *id && f.key.account == space.key.account));
        let Some(folder) = folder.filter(|folder| folder.spaces.len() > 1) else {
            done.insert(&space.key.id);
            if wanted(space) {
                out.push(Item::Space { index, child: None, open: None });
            }
            continue;
        };
        let members: Vec<(usize, &Space)> = folder
            .spaces
            .iter()
            .filter_map(|id| spaces.iter().enumerate().find(|(_, s)| s.key.id == *id && s.key.account == space.key.account))
            .collect();
        for (_, member) in &members {
            done.insert(&member.key.id);
        }
        let open = opened.contains(&folder.key.id);
        let children: Vec<(usize, &Space)> = members
            .iter()
            .copied()
            .filter(|(_, s)| if open { wanted(s) } else { s.attention > 0 || open_space == Some(s.key.id.as_str()) })
            .collect();
        let hidden_news = members.iter().any(|(_, s)| news(s) && !children.iter().any(|(_, shown)| shown.key == s.key));
        if unread && !members.iter().any(|(_, s)| news(s)) {
            continue;
        }
        let name = if folder.name.is_empty() {
            members.iter().take(3).map(|(_, s)| s.name.as_str()).collect::<Vec<_>>().join(", ")
        } else {
            folder.name.clone()
        };
        let mention = members.iter().any(|(_, s)| s.attention > 0) && hidden_news;
        out.push(Item::Folder { id: folder.key.id.clone(), name, color: folder.color, open, lit: hidden_news, mention });
        let color = folder.color.unwrap_or(0);
        out.extend(children.into_iter().map(|(index, _)| Item::Space { index, child: Some(color), open: None }));
    }
    out
}

/// The Telegram home: the Archive row first in All, then the chats (Telegram's pins first,
/// then newest; chatd's order), each once.
pub fn telegram(home: &Home) -> Vec<Item> {
    let mut out = Vec::new();
    if home.folder.is_none() && home.archive_total > 0 {
        let names: Vec<&str> = home.archive.iter().take(3).map(|c| c.title.as_str()).collect();
        let mention = home.archive.iter().any(|c| c.mentions > 0);
        let unread = home.archive.iter().any(|c| c.badge() != Badge::None);
        out.push(Item::Action { act: Act::Archive, icon: "icons/inbox.svg", words: "Archive".into(), after: names.join(", "), dot: (mention || unread).then_some(mention) });
    }
    let pinned = home.chats.iter().any(|c| c.pin.is_some());
    if pinned {
        header(&mut out, "Pinned");
        out.extend(home.chats.iter().filter(|c| c.pin.is_some()).cloned().map(Item::Chat));
        header(&mut out, "Chats");
    }
    out.extend(home.chats.iter().filter(|c| c.pin.is_none()).cloned().map(Item::Chat));
    out
}

/// The Signal home: the chats, starred ones first. Signal has no Spaces, folders or archive a
/// linked device hears of.
pub fn signal(home: &Home) -> Vec<Item> {
    let mut out = Vec::new();
    let starred = home.chats.iter().any(|c| c.starred);
    if starred {
        header(&mut out, "Pinned");
        out.extend(home.chats.iter().filter(|c| c.starred).cloned().map(Item::Chat));
        header(&mut out, "Chats");
    }
    out.extend(home.chats.iter().filter(|c| !c.starred).cloned().map(Item::Chat));
    out
}

// CONTRACT: `Space` says nothing of a team being archived, so the "Archived teams" fold of
// `03` §5 is not drawn; archived teams list with the rest.
/// The Teams home: Pinned, Chats (never collapsed, at least eight), the Meetings fold, then the
/// teams, collapsed, each showing the channels that need me (rule 3) or all when opened.
pub fn teams(home: &Home, spaces: &[Space], channels: &dyn Fn(&Key) -> Vec<Rc<Conversation>>, open: Option<&Key>, now: i64) -> Vec<Item> {
    let mut out = Vec::new();
    let pinned: Vec<&Rc<Conversation>> = home.chats.iter().filter(|c| c.starred || c.pin.is_some()).collect();
    if !pinned.is_empty() {
        header(&mut out, "Pinned");
        out.extend(pinned.iter().map(|c| Item::Chat(Rc::clone(c))));
    }
    let rest: Vec<&Rc<Conversation>> = home.chats.iter().filter(|c| !(c.starred || c.pin.is_some())).collect();
    let quiet_meeting = |c: &Conversation| c.meeting && now - c.updated > MEETING_QUIET && c.badge() == Badge::None && open != Some(&c.key);
    let (meetings, chats): (Vec<&Rc<Conversation>>, Vec<&Rc<Conversation>>) = rest.into_iter().partition(|c| quiet_meeting(c));
    // My own Notes close the chats, as in Teams, whatever its last message.
    let (notes, chats): (Vec<&Rc<Conversation>>, Vec<&Rc<Conversation>>) = chats.into_iter().partition(|c| c.special == Special::Saved);
    let keep = |(ix, c): &(usize, &&Rc<Conversation>)| {
        home.chats_all || *ix < CHATS_SHOWN || c.badge() != Badge::None || !c.draft.is_empty() || now - c.updated <= CHATS_ACTIVE || open == Some(&c.key)
    };
    let shown: Vec<&Rc<Conversation>> = chats.iter().enumerate().filter(keep).map(|(_, c)| *c).collect();
    if !chats.is_empty() || !meetings.is_empty() || !notes.is_empty() {
        header(&mut out, "Chats");
        out.extend(shown.iter().chain(&notes).map(|c| Item::Chat(Rc::clone(c))));
        if chats.len() > shown.len() {
            out.push(Item::Action { act: Act::MoreChats, icon: "icons/chevron-down.svg", words: format!("Show {} more", chats.len() - shown.len()), after: String::new(), dot: None });
        }
        if !meetings.is_empty() {
            let names: Vec<&str> = meetings.iter().take(3).map(|c| c.title.as_str()).collect();
            let dot = meetings.iter().any(|c| c.badge() != Badge::None).then_some(false);
            out.push(Item::Action { act: Act::Meetings, icon: "icons/calendar.svg", words: "Meetings".into(), after: names.join(", "), dot });
            if home.meetings_open {
                out.extend(meetings.iter().map(|c| Item::Chat(Rc::clone(c))));
            }
        }
    }
    let teams: Vec<(usize, &Space)> = spaces.iter().enumerate().filter(|(_, s)| s.key.network == Network::Teams).collect();
    if !teams.is_empty() {
        header(&mut out, "Teams");
        for (index, team) in teams {
            let opened = home.opened.contains(&team.key.id);
            out.push(Item::Space { index, child: None, open: Some(opened) });
            let listed = channels(&team.key);
            let shown = listed.into_iter().filter(|c| opened || c.badge() != Badge::None || open == Some(&c.key));
            out.extend(shown.map(Item::Channel));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ConversationKind;

    fn key(id: &str) -> Key {
        Key { network: Network::Discord, account: "me".into(), id: id.into() }
    }

    fn chat(id: &str, unread: u32) -> Rc<Conversation> {
        let mut c = crate::gui::sidebar::tests::row(id, ConversationKind::Direct);
        c.unread = unread;
        Rc::new(c)
    }

    fn space(id: &str, folder: Option<&str>, attention: u32, unread: bool) -> Space {
        Space { key: key(id), name: id.into(), icon: None, position: 0, attention, unread, folder: folder.map(Into::into), muted: false, hide_muted: false }
    }

    fn folder(id: &str, spaces: &[&str]) -> Folder {
        Folder { key: key(id), name: id.into(), position: 0, color: Some(0x5865f2), spaces: spaces.iter().map(|s| (*s).to_owned()).collect(), unread: 0, mentions: 0 }
    }

    fn shape(items: &[Item], spaces: &[Space]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Header { label, .. } => format!("[{label}]"),
                Item::Chat(c) | Item::Voice(c) | Item::Channel(c) => c.key.id.clone(),
                Item::Action { words, .. } => format!("<{words}>"),
                Item::Space { index, child, .. } => format!("{}{}", if child.is_some() { "." } else { "" }, spaces[*index].name),
                Item::Folder { name, open, lit, .. } => format!("{}{name}{}", if *open { "v" } else { ">" }, if *lit { "*" } else { "" }),
            })
            .collect()
    }

    #[test]
    fn dms_show_every_one_with_a_number_then_the_newest_up_to_ten() {
        let mut chats: Vec<_> = (0..14).map(|n| chat(&format!("d{n}"), 0)).collect();
        chats[12] = chat("waiting", 2);
        let (shown, more) = shown_dms(&chats, false, false);
        assert_eq!(shown.len(), 10);
        assert_eq!(shown.last().unwrap().key.id, "waiting", "an older DM with a number is still shown");
        assert_eq!(shown[8].key.id, "d8");
        assert_eq!(more, 4);
        assert_eq!(shown_dms(&chats, true, false).0.len(), 14);
        assert_eq!(shown_dms(&chats, false, true).0.len(), 1);
    }

    #[test]
    fn a_collapsed_folder_still_lists_the_open_server_and_any_with_a_number() {
        let spaces = [
            space("loose", None, 0, false),
            space("a", Some("trail"), 0, true),
            space("b", Some("trail"), 2, false),
            space("c", Some("trail"), 0, false),
            space("d", Some("trail"), 0, false),
        ];
        let folders = [folder("trail", &["a", "b", "c", "d"])];
        let closed = servers(&spaces, &folders, &HashSet::new(), Some("d"), false);
        // `a` is plain unread and hidden: the folder itself lights up.
        assert_eq!(shape(&closed, &spaces), ["loose", ">trail*", ".b", ".d"]);
        let opened = servers(&spaces, &folders, &HashSet::from(["trail".to_owned()]), None, false);
        assert_eq!(shape(&opened, &spaces), ["loose", "vtrail", ".a", ".b", ".c", ".d"]);
        // Unread keeps only what has news.
        let news = servers(&spaces, &folders, &HashSet::from(["trail".to_owned()]), None, true);
        assert_eq!(shape(&news, &spaces), ["vtrail", ".a", ".b"]);
    }

    #[test]
    fn the_discord_home_reads_friends_requests_voice_dms_then_servers() {
        let spaces = [space("moss", None, 0, false)];
        let home = Home {
            chats: vec![chat("ines", 1), chat("kofi", 0)],
            in_voice: (0..5).map(|n| chat(&format!("v{n}"), 0)).collect(),
            requests: 2,
            online: 6,
            ..Home::for_network(Network::Discord)
        };
        let got = shape(&discord(&home, &spaces, None, false), &spaces);
        assert_eq!(got, ["<Friends>", "<Message requests>", "[In voice]", "v0", "v1", "v2", "<2 more>", "[Direct messages]", "ines", "kofi", "[Servers]", "moss"]);
        assert_eq!(shape(&discord(&home, &spaces, None, true), &spaces), ["[Direct messages]", "ines"]);
    }

    #[test]
    fn teams_keeps_quiet_meetings_in_their_fold_and_shows_unread_channels_of_closed_teams() {
        const NOW: i64 = 100 * 86_400;
        let at = |id: &str, days: i64, meeting: bool, unread: u32| {
            let mut c = (*chat(id, unread)).clone();
            (c.updated, c.meeting) = (NOW - days * 86_400, meeting);
            Rc::new(c)
        };
        let home = Home {
            chats: vec![at("ana", 0, false, 0), at("standup", 2, true, 0), at("review", 20, true, 0), at("offsite", 30, true, 1)],
            ..Home::for_network(Network::Teams)
        };
        let team = Space { key: Key { network: Network::Teams, ..key("t") }, ..space("Platform", None, 0, true) };
        let channels = |_: &Key| vec![at("general", 1, false, 0), at("releases", 1, false, 3)];
        let got = shape(&teams(&home, std::slice::from_ref(&team), &channels, None, NOW), std::slice::from_ref(&team));
        assert_eq!(got, ["[Chats]", "ana", "standup", "offsite", "<Meetings>", "[Teams]", "Platform", "releases"]);
    }

    #[test]
    fn signal_lists_its_chats_with_the_starred_first() {
        let mut grace = (*chat("grace", 1)).clone();
        grace.starred = true;
        let home = Home { chats: vec![chat("club", 0), Rc::new(grace)], ..Home::for_network(Network::Signal) };
        assert_eq!(shape(&signal(&home), &[]), ["[Pinned]", "grace", "[Chats]", "club"]);
        let plain = Home { chats: vec![chat("club", 0)], ..Home::for_network(Network::Signal) };
        assert_eq!(shape(&signal(&plain), &[]), ["club"]);
    }

    #[test]
    fn teams_notes_close_the_chats_however_new() {
        let mut notes = (*chat("notes", 0)).clone();
        (notes.special, notes.updated) = (Special::Saved, 99);
        let mut ana = (*chat("ana", 0)).clone();
        ana.updated = 50;
        let home = Home { chats: vec![Rc::new(notes), Rc::new(ana)], ..Home::for_network(Network::Teams) };
        let got = shape(&teams(&home, &[], &|_: &Key| Vec::new(), None, 100), &[]);
        assert_eq!(got, ["[Chats]", "ana", "notes"]);
    }
}
