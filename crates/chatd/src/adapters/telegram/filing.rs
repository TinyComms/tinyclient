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
    /// When this chat's own mute ends, as Unix seconds. `None` is the kind's default.
    /// [`i32::MAX`] is muted until it is turned off.
    pub mute_until: Option<i64>,
}

/// Whether a Telegram `mute_until` still silences the chat. [`i32::MAX`] does not expire.
pub(super) fn mute_holds(until: i32) -> bool {
    until == i32::MAX || i64::from(until) > crate::hub::now()
}

/// The chat's own `mute_until`, when Telegram named one.
pub(super) fn mute_stamp(settings: &tl::enums::PeerNotifySettings) -> Option<i64> {
    let tl::enums::PeerNotifySettings::Settings(settings) = settings;
    settings.mute_until.map(i64::from)
}

fn clamp_stamp(until: i64) -> i32 {
    until.clamp(0, i64::from(i32::MAX)) as i32
}

/// A timed mute that has ended is not muted, the next time this chat is read. No timer.
pub(super) fn refresh_timed_mute(account: &Account, who: Who) {
    let (until, was) = {
        let live = account.live();
        match live.filed.facts.get(&who) {
            Some(facts) => (facts.mute_until, facts.muted),
            None => return,
        }
    };
    let Some(until) = until else {
        return;
    };
    let muted = mute_holds(clamp_stamp(until));
    if muted == was {
        return;
    }
    let _ = account.ctx.muted(&who.text(), muted);
    facts_changed(account, who, |facts| facts.muted = muted);
}

/// Where the next page of a list starts: the last dialog's date, message and peer.
pub(super) type Cursor = (i32, i32, Who);

/// Everything filing needs, held while connected.
#[derive(Default)]
pub(super) struct Filed {
    pub rules: Vec<Rule>,
    pub facts: HashMap<Who, Facts>,
    /// The chat on screen and its read state when it opened. A folder that excludes read
    /// chats keeps it until it is left: while it is open, its read fact stands as it was.
    pub open: Option<(Who, bool)>,
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
        let mut facts = self.facts.get(&who).copied().unwrap_or_default();
        if let Some(until) = facts.mute_until {
            if !mute_holds(clamp_stamp(until)) {
                facts.muted = false;
            }
        }
        if let Some((_, read)) = self.open.filter(|(held, _)| *held == who) {
            facts.read = read;
        }
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

/// Pins/unpins, archives/unarchives, or adds/removes a Telegram dialog from a folder.
pub(super) fn file(
    account: &Account,
    conversation: &str,
    pinned: Option<bool>,
    archived: Option<bool>,
    folder_add: Option<&str>,
    folder_remove: Option<&str>,
) -> Result<(), Fault> {
    let Some((who, None)) = Who::parse(conversation) else {
        return Err(Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None });
    };
    let peer = account.input(who)?;

    if let Some(archive) = archived {
        account.ask(&tl::functions::folders::EditPeerFolders {
            folder_peers: vec![tl::enums::InputFolderPeer::Peer(tl::types::InputFolderPeer {
                peer: peer.clone(),
                folder_id: i32::from(archive),
            })],
        })?;
        let moved = {
            let mut live = account.live();
            let entry = live.filed.facts.entry(who).or_default();
            entry.archived = archive;
            let other = usize::from(!archive);
            let mut affected = live.filed.pins[other].clone();
            live.filed.pins[other].retain(|held| *held != who);
            affected.push(who);
            affected
        };
        refile(account, moved);
    }

    if let Some(pin) = pinned {
        account.ask(&tl::functions::messages::ToggleDialogPin {
            pinned: pin,
            peer: tl::enums::InputDialogPeer::Peer(tl::types::InputDialogPeer { peer: peer.clone() }),
        })?;
        let moved = {
            let mut live = account.live();
            let list = usize::from(live.filed.facts.get(&who).is_some_and(|f| f.archived));
            let pins = &mut live.filed.pins[list];
            let before = pins.clone();
            pins.retain(|held| *held != who);
            if pin {
                pins.insert(0, who);
            }
            before.into_iter().chain(std::iter::once(who)).collect::<Vec<_>>()
        };
        refile(account, moved);
    }

    for (target, add) in [(folder_add, true), (folder_remove, false)] {
        let Some(folder_str) = target else {
            continue;
        };
        let folder_id: i32 = folder_str
            .parse()
            .map_err(|_| Fault::Rpc { code: 400, name: "FILTER_ID_INVALID".to_owned(), value: None })?;
        let tl::enums::messages::DialogFilters::Filters(found) = account.ask(&tl::functions::messages::GetDialogFilters {})?;
        let mut updated = None;
        for filter in found.filters {
            match filter {
                tl::enums::DialogFilter::Filter(mut f) if f.id == folder_id => {
                    if add {
                        f.exclude_peers.retain(|p| input_who(p, account.me) != Some(who));
                        if !f.include_peers.iter().any(|p| input_who(p, account.me) == Some(who)) {
                            f.include_peers.push(peer.clone());
                        }
                    } else {
                        f.include_peers.retain(|p| input_who(p, account.me) != Some(who));
                        f.pinned_peers.retain(|p| input_who(p, account.me) != Some(who));
                    }
                    updated = Some(tl::enums::DialogFilter::Filter(f));
                    break;
                }
                tl::enums::DialogFilter::Chatlist(mut c) if c.id == folder_id => {
                    if add {
                        if !c.include_peers.iter().any(|p| input_who(p, account.me) == Some(who)) {
                            c.include_peers.push(peer.clone());
                        }
                    } else {
                        c.include_peers.retain(|p| input_who(p, account.me) != Some(who));
                        c.pinned_peers.retain(|p| input_who(p, account.me) != Some(who));
                    }
                    updated = Some(tl::enums::DialogFilter::Chatlist(c));
                    break;
                }
                _ => {}
            }
        }
        let Some(new_filter) = updated else {
            return Err(Fault::Rpc { code: 400, name: "FILTER_ID_INVALID".to_owned(), value: None });
        };
        account.ask(&tl::functions::messages::UpdateDialogFilter { id: folder_id, filter: Some(new_filter.clone()) })?;
        if let Some(new_rule) = rule(&new_filter, account.me) {
            let mut live = account.live();
            if let Some(slot) = live.filed.rules.iter_mut().find(|r| r.id == folder_id) {
                *slot = new_rule;
            }
        }
        report(account);
        refile(account, [who]);
    }

    Ok(())
}

fn resolve_peers(account: &Account, ids: &[String]) -> Result<Vec<tl::enums::InputPeer>, Fault> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some((who, None)) = Who::parse(id) else {
            return Err(Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None });
        };
        out.push(account.input(who)?);
    }
    Ok(out)
}

fn spec_from_filter(filter: &tl::enums::DialogFilter, me: i64) -> Option<protocol::FolderSpec> {
    let ids = |peers: &[tl::enums::InputPeer]| {
        peers.iter().filter_map(|p| input_who(p, me).map(Who::text)).collect::<Vec<String>>()
    };
    match filter {
        tl::enums::DialogFilter::Filter(f) => Some(protocol::FolderSpec {
            id: Some(f.id.to_string()),
            name: title(&f.title),
            emoticon: f.emoticon.clone(),
            include: ids(&f.include_peers),
            exclude: ids(&f.exclude_peers),
            pinned: ids(&f.pinned_peers),
            contacts: f.contacts,
            non_contacts: f.non_contacts,
            groups: f.groups,
            broadcasts: f.broadcasts,
            bots: f.bots,
            exclude_muted: f.exclude_muted,
            exclude_read: f.exclude_read,
            exclude_archived: f.exclude_archived,
        }),
        tl::enums::DialogFilter::Chatlist(c) => Some(protocol::FolderSpec {
            id: Some(c.id.to_string()),
            name: title(&c.title),
            emoticon: c.emoticon.clone(),
            include: ids(&c.include_peers),
            pinned: ids(&c.pinned_peers),
            ..protocol::FolderSpec::default()
        }),
        tl::enums::DialogFilter::Default => None,
    }
}

fn parse_folder_id(raw: &str) -> Result<i32, Fault> {
    let id = raw
        .parse::<i32>()
        .map_err(|_| Fault::Rpc { code: 400, name: "FILTER_ID_INVALID".to_owned(), value: None })?;
    if !(2..=255).contains(&id) {
        return Err(Fault::Rpc { code: 400, name: "FILTER_ID_INVALID".to_owned(), value: None });
    }
    Ok(id)
}

/// Creates or edits a Telegram chat folder (`messages.updateDialogFilter`).
pub(super) fn save_folder(account: &Account, spec: &protocol::FolderSpec) -> Result<protocol::Folder, Fault> {
    let name = spec.name.trim().to_owned();
    if name.is_empty() {
        return Err(Fault::Rpc { code: 400, name: "FILTER_TITLE_EMPTY".to_owned(), value: None });
    }
    if name.chars().count() > 12 {
        return Err(Fault::Rpc { code: 400, name: "FILTER_TITLE_TOO_LONG".to_owned(), value: None });
    }
    let any_kind = spec.contacts || spec.non_contacts || spec.groups || spec.broadcasts || spec.bots;
    let any_peer = !spec.include.is_empty() || !spec.pinned.is_empty();
    if !any_kind && !any_peer {
        return Err(Fault::Rpc { code: 400, name: "FILTER_INCLUDE_EMPTY".to_owned(), value: None });
    }
    let editing = spec.id.is_some();
    let id = match &spec.id {
        Some(raw) => parse_folder_id(raw)?,
        None => {
            let live = account.live();
            let used: HashSet<i32> = live.filed.rules.iter().map(|r| r.id).collect();
            (2..=255)
                .find(|cand| !used.contains(cand))
                .ok_or_else(|| Fault::Rpc { code: 400, name: "FILTERS_TOO_MUCH".to_owned(), value: None })?
        }
    };
    let include_peers = resolve_peers(account, &spec.include)?;
    let exclude_peers = resolve_peers(account, &spec.exclude)?;
    let pinned_peers = resolve_peers(account, &spec.pinned)?;
    let title_entities = tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
        text: name.clone(),
        entities: Vec::new(),
    });
    // Preserve shared Chatlist metadata when editing an existing DialogFilterChatlist.
    let existing_chatlist = if editing {
        match account.ask(&tl::functions::messages::GetDialogFilters {}) {
            Ok(tl::enums::messages::DialogFilters::Filters(found)) => found.filters.into_iter().find_map(|f| match f {
                tl::enums::DialogFilter::Chatlist(c) if c.id == id => Some(c),
                _ => None,
            }),
            _ => None,
        }
    } else {
        None
    };
    let filter = if let Some(mut chatlist) = existing_chatlist {
        chatlist.title = title_entities;
        chatlist.emoticon = spec.emoticon.clone();
        chatlist.pinned_peers = pinned_peers;
        chatlist.include_peers = include_peers;
        tl::enums::DialogFilter::Chatlist(chatlist)
    } else {
        tl::enums::DialogFilter::Filter(tl::types::DialogFilter {
            contacts: spec.contacts,
            non_contacts: spec.non_contacts,
            groups: spec.groups,
            broadcasts: spec.broadcasts,
            bots: spec.bots,
            exclude_muted: spec.exclude_muted,
            exclude_read: spec.exclude_read,
            exclude_archived: spec.exclude_archived,
            title_noanimate: false,
            id,
            title: title_entities,
            emoticon: spec.emoticon.clone(),
            color: None,
            pinned_peers,
            include_peers,
            exclude_peers,
        })
    };
    account.ask(&tl::functions::messages::UpdateDialogFilter { id, filter: Some(filter.clone()) })?;
    let position = {
        let mut live = account.live();
        if let Some(new_rule) = rule(&filter, account.me) {
            if let Some(pos) = live.filed.rules.iter().position(|r| r.id == id) {
                live.filed.rules[pos] = new_rule;
                pos as i32
            } else {
                live.filed.rules.push(new_rule);
                (live.filed.rules.len() - 1) as i32
            }
        } else {
            0
        }
    };
    report(account);
    refile(account, everyone(account));
    Ok(protocol::Folder {
        key: protocol::Key::new(&account.ctx.account, id.to_string()),
        name,
        position,
        color: None,
        spaces: Vec::new(),
        unread: 0,
        mentions: 0,
    })
}

/// Deletes a Telegram chat folder (`messages.updateDialogFilter` with `filter: None`).
pub(super) fn delete_folder(account: &Account, folder_id: &str) -> Result<(), Fault> {
    let id = parse_folder_id(folder_id)?;
    account.ask(&tl::functions::messages::UpdateDialogFilter { id, filter: None })?;
    {
        let mut live = account.live();
        live.filed.rules.retain(|r| r.id != id);
    }
    report(account);
    refile(account, everyone(account));
    Ok(())
}

/// Reorders Telegram chat folders (`messages.updateDialogFiltersOrder`).
pub(super) fn reorder_folders(account: &Account, order: &[String]) -> Result<(), Fault> {
    let mut ids = Vec::with_capacity(order.len());
    for raw in order {
        ids.push(parse_folder_id(raw)?);
    }
    account.ask(&tl::functions::messages::UpdateDialogFiltersOrder { order: ids.clone() })?;
    {
        let mut live = account.live();
        live.filed
            .rules
            .sort_by_key(|rule| ids.iter().position(|id| *id == rule.id).unwrap_or(usize::MAX));
    }
    report(account);
    Ok(())
}

/// Fetches Telegram's suggested chat folders (`messages.getSuggestedDialogFilters`).
pub(super) fn suggested_folders(account: &Account) -> Result<Vec<protocol::SuggestedFolder>, Fault> {
    let found = account.ask(&tl::functions::messages::GetSuggestedDialogFilters {})?;
    let mut out = Vec::new();
    for item in found {
        let tl::enums::DialogFilterSuggested::Suggested(suggested) = item;
        if let Some(mut spec) = spec_from_filter(&suggested.filter, account.me) {
            spec.id = None;
            out.push(protocol::SuggestedFolder { spec, description: suggested.description });
        }
    }
    Ok(out)
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
