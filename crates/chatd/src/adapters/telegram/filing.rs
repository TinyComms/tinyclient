//! How Telegram files its chats: folders (filters over the chat list, each with its own pins),
//! the archive, and the pins of the main list and the archive. Everything here is kept small and
//! in memory once the dialogs are read, so a pin or a folder changing is told to the store row by
//! row, from what is held, never by reading every dialog again.

use super::peers::{Known, Who};
use super::wire::Fault;
use super::Account;
use crate::store::{Filing, FolderRow};
use grammers_tl_types as tl;
use std::collections::{HashMap, HashSet};

/// Most chats remembered for filing. Past this the oldest-read are filed from the store as it
/// stands until the next sign-on.
const FILED: usize = 20_000;

/// One folder's rules, as Telegram gives them.
#[derive(Debug, Clone, Default)]
pub(super) struct Rule {
    pub id: i32,
    pub title: String,
    /// Chats named in it, and its own pins in their order (pins are named too).
    pub named: HashSet<Who>,
    pub pinned: Vec<Who>,
    pub left_out: HashSet<Who>,
    pub contacts: bool,
    pub strangers: bool,
    pub groups: bool,
    pub broadcasts: bool,
    pub bots: bool,
    pub exclude_muted: bool,
    pub exclude_read: bool,
    pub exclude_archived: bool,
}

/// What a chat's filing depends on beyond who it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Facts {
    pub archived: bool,
    pub muted: bool,
    /// Nothing unread when it was last told.
    pub read: bool,
}

/// Where the next page of a list starts: the last dialog's date, message and peer.
pub(super) type Cursor = (i32, i32, Who);

/// Everything filing needs, held while connected.
#[derive(Default)]
pub(super) struct Filed {
    pub rules: Vec<Rule>,
    pub facts: HashMap<Who, Facts>,
    /// The pins of the main list and of the archive, in Telegram's order.
    pub pins: [Vec<Who>; 2],
    /// The chats of each list in the order they were read, for knowing when the user has
    /// scrolled near the end of what is loaded.
    pub order: [Vec<Who>; 2],
    /// Where each list continues; `None` once Telegram has no more.
    pub ends: [Option<Cursor>; 2],
}

impl Filed {
    pub fn note(&mut self, who: Who, facts: Facts) {
        if self.facts.len() >= FILED && !self.facts.contains_key(&who) {
            return;
        }
        self.facts.insert(who, facts);
    }

    /// Where a chat is filed now.
    pub fn filing(&self, who: Who, known: &Known) -> Filing {
        let facts = self.facts.get(&who).copied().unwrap_or_default();
        let list = usize::from(facts.archived);
        Filing {
            folders: self.rules.iter().filter(|rule| rule.holds(who, known, facts)).map(|rule| rule.id.to_string()).collect(),
            archived: facts.archived,
            pin: self.pins[list].iter().position(|pinned| *pinned == who).map(|at| at as i32),
            hidden: false,
            followed: false,
        }
    }

    pub fn rows(&self) -> Vec<FolderRow> {
        self.rules
            .iter()
            .enumerate()
            .map(|(at, rule)| FolderRow {
                id: rule.id.to_string(),
                name: rule.title.clone(),
                position: at as i32,
                // Telegram's folder colour is one of its own seven, not a colour.
                color: None,
                spaces: Vec::new(),
                pinned: rule.pinned.iter().map(|who| who.text()).collect(),
            })
            .collect()
    }
}

impl Rule {
    /// Whether a chat is in this folder. Chats it names are in it whatever the other rules say;
    /// the `exclude_*` rules narrow only the kinds it takes in. Unverified against Telegram's
    /// own apps for a named chat that is also muted or read.
    pub fn holds(&self, who: Who, known: &Known, facts: Facts) -> bool {
        if self.left_out.contains(&who) {
            return false;
        }
        if self.named.contains(&who) {
            return true;
        }
        let kind = match who {
            Who::User(_) if known.bot => self.bots,
            Who::User(_) if known.contact => self.contacts,
            Who::User(_) => self.strangers,
            Who::Channel(_) if known.broadcast => self.broadcasts,
            _ => self.groups,
        };
        kind && !(self.exclude_muted && facts.muted) && !(self.exclude_read && facts.read) && !(self.exclude_archived && facts.archived)
    }
}

fn input_who(peer: &tl::enums::InputPeer, me: i64) -> Option<Who> {
    Some(match peer {
        tl::enums::InputPeer::PeerSelf => Who::User(me),
        tl::enums::InputPeer::User(user) => Who::User(user.user_id),
        tl::enums::InputPeer::Chat(chat) => Who::Group(chat.chat_id),
        tl::enums::InputPeer::Channel(channel) => Who::Channel(channel.channel_id),
        _ => return None,
    })
}

fn title(title: &tl::enums::TextWithEntities) -> String {
    let tl::enums::TextWithEntities::Entities(title) = title;
    title.text.clone()
}

/// One of Telegram's dialog filters as a folder. The default (All) is no folder.
pub(super) fn rule(filter: &tl::enums::DialogFilter, me: i64) -> Option<Rule> {
    let whos = |peers: &[tl::enums::InputPeer]| peers.iter().filter_map(|peer| input_who(peer, me)).collect::<Vec<Who>>();
    Some(match filter {
        tl::enums::DialogFilter::Filter(filter) => {
            let pinned = whos(&filter.pinned_peers);
            Rule {
                id: filter.id,
                title: title(&filter.title),
                named: pinned.iter().copied().chain(whos(&filter.include_peers)).collect(),
                pinned,
                left_out: whos(&filter.exclude_peers).into_iter().collect(),
                contacts: filter.contacts,
                strangers: filter.non_contacts,
                groups: filter.groups,
                broadcasts: filter.broadcasts,
                bots: filter.bots,
                exclude_muted: filter.exclude_muted,
                exclude_read: filter.exclude_read,
                exclude_archived: filter.exclude_archived,
            }
        }
        tl::enums::DialogFilter::Chatlist(list) => {
            let pinned = whos(&list.pinned_peers);
            Rule {
                id: list.id,
                title: title(&list.title),
                named: pinned.iter().copied().chain(whos(&list.include_peers)).collect(),
                pinned,
                ..Rule::default()
            }
        }
        tl::enums::DialogFilter::Default => return None,
    })
}

/// The folders, one request.
pub(super) fn ask_rules(account: &Account) -> Result<Vec<Rule>, Fault> {
    let tl::enums::messages::DialogFilters::Filters(found) = account.ask(&tl::functions::messages::GetDialogFilters {})?;
    Ok(found.filters.iter().filter_map(|filter| rule(filter, account.me)).collect())
}

/// The folder list, as it is held, to the store.
pub(super) fn report(account: &Account) {
    let rows = account.live().filed.rows();
    if let Err(err) = account.ctx.folders(&rows) {
        tracing::warn!("chatd: Telegram: folders: {err}");
    }
}

/// Tells the store where these chats are filed now, one row at a time; a row whose filing did
/// not change costs one comparison in the store and announces nothing.
pub(super) fn refile(account: &Account, whos: impl IntoIterator<Item = Who>) {
    let filings: Vec<(String, Filing)> = {
        let dir = account.dir();
        let live = account.live();
        let mut seen = HashSet::new();
        whos.into_iter()
            .filter(|who| seen.insert(*who))
            .filter_map(|who| Some((who.text(), live.filed.filing(who, dir.get(who)?))))
            .collect()
    };
    for (id, filing) in filings {
        if let Err(err) = account.ctx.filing(&id, &filing) {
            tracing::warn!("chatd: Telegram: filing: {err}");
        }
    }
}

/// Every chat held, for a change to the folders themselves.
fn everyone(account: &Account) -> Vec<Who> {
    account.live().filed.facts.keys().copied().collect()
}

fn list_of(folder: Option<i32>) -> usize {
    usize::from(folder == Some(1))
}

fn peer_of(peer: &tl::enums::DialogPeer) -> Option<Who> {
    match peer {
        tl::enums::DialogPeer::Peer(peer) => Some(Who::of(&peer.peer)),
        tl::enums::DialogPeer::Folder(_) => None,
    }
}

/// A pin or a folder changed: what Telegram said is applied to what is held. At most one
/// request, and only where the update does not carry the new state itself.
pub(super) fn apply(account: &Account, update: &tl::enums::Update) -> bool {
    use tl::enums::Update;
    match update {
        // One chat pinned or unpinned: it goes to the top of the pins or leaves them.
        Update::DialogPinned(pinned) => {
            let Some(who) = peer_of(&pinned.peer) else {
                return true;
            };
            let list = list_of(pinned.folder_id);
            let moved = {
                let mut live = account.live();
                let pins = &mut live.filed.pins[list];
                let before = pins.clone();
                pins.retain(|held| *held != who);
                if pinned.pinned {
                    pins.insert(0, who);
                }
                before.into_iter().chain(std::iter::once(who)).collect::<Vec<_>>()
            };
            refile(account, moved);
        }
        Update::PinnedDialogs(pins) => {
            let list = list_of(pins.folder_id);
            let order: Vec<Who> = match &pins.order {
                Some(order) => order.iter().filter_map(peer_of).collect(),
                // Telegram did not say the order: the pins alone are asked, once.
                None => match pinned(account, pins.folder_id.unwrap_or(0)) {
                    Ok(order) => order,
                    Err(fault) => {
                        tracing::warn!("chatd: Telegram: pins: {fault}");
                        return true;
                    }
                },
            };
            let before = std::mem::replace(&mut account.live().filed.pins[list], order.clone());
            refile(account, before.into_iter().chain(order));
        }
        // Chats moved into or out of the archive.
        Update::FolderPeers(moved) => {
            let whos: Vec<Who> = {
                let mut live = account.live();
                moved
                    .folder_peers
                    .iter()
                    .map(|tl::enums::FolderPeer::Peer(peer)| {
                        let who = Who::of(&peer.peer);
                        let archived = peer.folder_id == 1;
                        let mut facts = live.filed.facts.get(&who).copied().unwrap_or_default();
                        facts.archived = archived;
                        live.filed.note(who, facts);
                        // A pin stays with the list it was made in.
                        live.filed.pins[usize::from(!archived)].retain(|held| *held != who);
                        who
                    })
                    .collect()
            };
            refile(account, whos);
        }
        // One folder made, changed or removed, with its rules.
        Update::DialogFilter(changed) => {
            {
                let mut live = account.live();
                let rules = &mut live.filed.rules;
                let made = changed.filter.as_ref().and_then(|filter| rule(filter, account.me));
                match (rules.iter().position(|held| held.id == changed.id), made) {
                    (Some(at), Some(made)) => rules[at] = made,
                    (None, Some(made)) => rules.push(made),
                    (Some(at), None) => drop(rules.remove(at)),
                    (None, None) => {}
                }
            }
            report(account);
            refile(account, everyone(account));
        }
        Update::DialogFilterOrder(order) => {
            {
                let mut live = account.live();
                let rules = &mut live.filed.rules;
                rules.sort_by_key(|rule| order.order.iter().position(|id| *id == rule.id).unwrap_or(usize::MAX));
            }
            report(account);
        }
        // Something about the folders changed and Telegram did not say what: they are asked,
        // once; the chats are not.
        Update::DialogFilters => {
            match ask_rules(account) {
                Ok(rules) => account.live().filed.rules = rules,
                Err(fault) => {
                    tracing::warn!("chatd: Telegram: folders: {fault}");
                    return true;
                }
            }
            report(account);
            refile(account, everyone(account));
        }
        _ => return false,
    }
    true
}

/// The pinned chats of one list, in order: one request.
fn pinned(account: &Account, folder_id: i32) -> Result<Vec<Who>, Fault> {
    let tl::enums::messages::PeerDialogs::Dialogs(found) = account.ask(&tl::functions::messages::GetPinnedDialogs { folder_id })?;
    account.learn(&found.users, &found.chats);
    Ok(found
        .dialogs
        .iter()
        .filter_map(|dialog| match dialog {
            tl::enums::Dialog::Dialog(dialog) => Some(Who::of(&dialog.peer)),
            tl::enums::Dialog::Folder(_) => None,
        })
        .collect())
}

/// A chat's filing changed because something about it did (muted, read): folders with
/// `exclude_muted` or `exclude_read` follow.
pub(super) fn facts_changed(account: &Account, who: Who, change: impl FnOnce(&mut Facts)) {
    let changed = {
        let mut live = account.live();
        let Some(facts) = live.filed.facts.get_mut(&who) else {
            return;
        };
        let before = *facts;
        change(facts);
        *facts != before && live.filed.rules.iter().any(|rule| rule.exclude_muted || rule.exclude_read)
    };
    if changed {
        refile(account, [who]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_takes_its_named_chats_and_kinds_minus_what_it_excludes() {
        let rule = Rule {
            id: 3,
            named: [Who::Group(1)].into_iter().collect(),
            left_out: [Who::User(9)].into_iter().collect(),
            contacts: true,
            exclude_muted: true,
            ..Rule::default()
        };
        let contact = Known { contact: true, ..Known::default() };
        let quiet = Facts { muted: true, ..Facts::default() };
        assert!(rule.holds(Who::Group(1), &Known::default(), quiet), "named chats are in whatever their state");
        assert!(rule.holds(Who::User(2), &contact, Facts::default()));
        assert!(!rule.holds(Who::User(2), &contact, quiet), "a muted contact is excluded");
        assert!(!rule.holds(Who::User(9), &contact, Facts::default()), "a left-out chat never is");
        assert!(!rule.holds(Who::Group(5), &Known::default(), Facts::default()));
    }

    #[test]
    fn a_chat_is_filed_in_every_folder_that_holds_it_with_its_pin() {
        let mut filed = Filed::default();
        let one = |id, named: &[Who]| Rule { id, named: named.iter().copied().collect(), ..Rule::default() };
        filed.rules = vec![one(2, &[Who::Group(1)]), one(3, &[Who::Group(1), Who::User(4)])];
        filed.pins[0] = vec![Who::User(4), Who::Group(1)];
        let filing = filed.filing(Who::Group(1), &Known::default());
        assert_eq!((filing.folders, filing.pin, filing.archived), (vec!["2".to_owned(), "3".to_owned()], Some(1), false));
        filed.note(Who::User(4), Facts { archived: true, ..Facts::default() });
        // An archived chat is pinned only among the archive's pins.
        assert_eq!(filed.filing(Who::User(4), &Known::default()).pin, None);
    }
}
