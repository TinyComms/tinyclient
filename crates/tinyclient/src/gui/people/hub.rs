//! What the shell watches, and what chatd says about it. One place gathers who is on screen
//! from every view and sends `Watch`; the presence and typing that come back are kept here,
//! bounded, for the views to look up as they draw. Avatar files are asked for the same way.

use crate::client::ask;
use crate::view::now;
use gpui::App;
use protocol::{
    AccountKey, Change, Key, MediaFile, Occupants, Person, Presence, RelateAction, Relation, Request, Response,
    Typist, Watching, ROWS_WATCHED, WATCH_LIMIT,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A change to what is on screen is sent once it has been still this long.
const QUIET: Duration = Duration::from_millis(200);
/// However long the changes keep coming (a held scroll), chatd hears within this.
const LATEST: Duration = Duration::from_millis(1000);
/// Avatar files asked for in one frame go in one `Fetch`.
const FETCH_QUIET: Duration = Duration::from_millis(40);
/// The shell drops a typist this long after hearing of them, whatever `until` says.
const TYPING_SECONDS: i64 = 12;
const PRESENCE_LIMIT: usize = 2000;
const PEOPLE_LIMIT: usize = 4000;
const FILE_LIMIT: usize = 2048;
/// A file that did not arrive is asked for again when it is next drawn this long after. chatd
/// answers at once and fetches nothing for a file that failed for good; one that failed for a
/// moment's trouble (the line was down) it fetches again.
const FILE_AGAIN: Duration = Duration::from_secs(60);
/// Voice channels whose occupants are remembered.
const VOICE_LIMIT: usize = 256;

/// Runs one request off the main thread and hands the answer back on it.
pub fn ask_later(cx: &mut App, request: Request, then: impl FnOnce(io::Result<Response>, &mut App) + 'static) {
    cx.spawn(async move |cx| {
        let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
        cx.update(|cx| then(answer, cx));
    })
    .detach();
}

#[derive(Debug, PartialEq)]
pub enum Due {
    /// Send this now; it has been recorded as sent.
    Send(Watching),
    /// Still changing: look again after this long.
    Wait(Duration),
    Nothing,
}

/// Who and what is on screen, per view, and what chatd was last told.
#[derive(Default)]
pub struct WatchSet {
    sources: BTreeMap<&'static str, Vec<Key>>,
    /// The conversation rows each list has on screen, for their previews.
    rows: BTreeMap<&'static str, Vec<Key>>,
    conversation: Option<Key>,
    members: Option<(u32, u32)>,
    space: Option<Key>,
    /// The window is the one being typed in.
    focused: bool,
    sent: Option<Watching>,
    /// When the unsent changes began and when the last one came.
    changing: Option<(Instant, Instant)>,
}

impl WatchSet {
    fn touched(&mut self, now: Instant) {
        self.changing = Some((self.changing.map_or(now, |(first, _)| first), now));
    }

    /// The people one view draws now, replacing what it drew before. True when that changed.
    pub fn people(&mut self, source: &'static str, people: Vec<Key>, now: Instant) -> bool {
        if people.is_empty() {
            if self.sources.remove(source).is_none() {
                return false;
            }
        } else if self.sources.get(source) == Some(&people) {
            return false;
        } else {
            self.sources.insert(source, people);
        }
        self.touched(now);
        true
    }

    /// The conversation rows one list draws now, replacing what it drew before.
    pub fn rows(&mut self, source: &'static str, rows: Vec<Key>, now: Instant) -> bool {
        if rows.is_empty() {
            if self.rows.remove(source).is_none() {
                return false;
            }
        } else if self.rows.get(source) == Some(&rows) {
            return false;
        } else {
            self.rows.insert(source, rows);
        }
        self.touched(now);
        true
    }

    /// Whether the window has focus: what arrives in the open conversation then is read.
    pub fn focused(&mut self, focused: bool, now: Instant) -> bool {
        if self.focused == focused {
            return false;
        }
        self.focused = focused;
        self.touched(now);
        true
    }

    pub fn conversation(&mut self, conversation: Option<Key>, now: Instant) -> bool {
        if self.conversation == conversation {
            return false;
        }
        self.conversation = conversation;
        self.members = None;
        self.touched(now);
        true
    }

    /// The visible rows of the open conversation's member list: first row and count.
    pub fn members(&mut self, members: Option<(u32, u32)>, now: Instant) -> bool {
        if self.members == members {
            return false;
        }
        self.members = members;
        self.touched(now);
        true
    }

    /// The Space whose tree is on screen.
    pub fn space(&mut self, space: Option<Key>, now: Instant) -> bool {
        if self.space == space {
            return false;
        }
        self.space = space;
        self.touched(now);
        true
    }

    /// Everything on screen: each person once, at most what chatd accepts.
    pub fn wanted(&self) -> Watching {
        let mut seen = HashSet::new();
        let people = self
            .sources
            .values()
            .flatten()
            .filter(|person| seen.insert(*person))
            .take(WATCH_LIMIT)
            .cloned()
            .collect();
        Watching {
            space: self.space.clone(),
            conversation: self.conversation.clone(),
            focused: self.focused,
            people,
            members: self.members,
            rows: {
                let mut seen = HashSet::new();
                self.rows.values().flatten().filter(|row| seen.insert(*row)).take(ROWS_WATCHED).cloned().collect()
            },
        }
    }

    pub fn is_watching(&self, person: &Key) -> bool {
        self.sources.values().flatten().any(|watched| watched == person)
    }

    /// chatd forgot what it was told (it restarted): the next `due` sends it again.
    pub fn forget(&mut self, now: Instant) {
        if self.sent.take().is_some() {
            self.touched(now);
        }
    }

    pub fn due(&mut self, now: Instant) -> Due {
        let Some((first, last)) = self.changing else {
            return Due::Nothing;
        };
        let wanted = self.wanted();
        let same_people = |sent: &Watching| {
            sent.people.len() == wanted.people.len()
                && sent.people.iter().collect::<HashSet<_>>() == wanted.people.iter().collect::<HashSet<_>>()
        };
        let unchanged = match &self.sent {
            Some(sent) => {
                sent.conversation == wanted.conversation
                    && sent.members == wanted.members
                    && sent.space == wanted.space
                    && sent.focused == wanted.focused
                    && sent.rows == wanted.rows
                    && same_people(sent)
            }
            // Nothing on screen and nothing ever sent: chatd has nothing to drop.
            None => wanted == Watching::default(),
        };
        if unchanged {
            self.changing = None;
            return Due::Nothing;
        }
        let quiet = now.saturating_duration_since(last);
        let waited = now.saturating_duration_since(first);
        let viewport_changed = self.sent.as_ref().map_or(wanted.members.is_some(), |sent| sent.members != wanted.members);
        let quiet_limit = if viewport_changed { Duration::from_millis(16) } else { QUIET };
        let latest_limit = if viewport_changed { Duration::from_millis(50) } else { LATEST };
        if quiet < quiet_limit && waited < latest_limit {
            return Due::Wait((quiet_limit - quiet).min(latest_limit - waited));
        }
        self.changing = None;
        self.sent = Some(wanted.clone());
        Due::Send(wanted)
    }
}

#[derive(Clone)]
struct Seen {
    presence: Presence,
    status: String,
}

enum Fetched {
    /// `Fetch` was sent; the file lands as a `Media` change.
    Asked,
    Here(String),
    /// The network could not give it, at this moment. The fallback stays, and the file is asked
    /// for again once [`FILE_AGAIN`] has passed.
    Failed(Instant),
}

impl Fetched {
    /// What a `Media` change or a `Fetch` answer says of a file: here, or not arriving.
    fn of(file: &MediaFile, at: Instant) -> Option<Fetched> {
        match (&file.path, &file.failed) {
            (Some(path), _) => Some(Fetched::Here(path.clone())),
            (None, Some(_)) => Some(Fetched::Failed(at)),
            (None, None) => None,
        }
    }

    /// Whether the file should be asked for now.
    fn ask_again(&self, at: Instant) -> bool {
        matches!(self, Fetched::Failed(failed) if at.duration_since(*failed) >= FILE_AGAIN)
    }
}

/// Repaints the window.
type Notify = Rc<dyn Fn(&mut App)>;

#[derive(Default)]
struct Inner {
    watch: WatchSet,
    watch_pending: bool,
    presence: HashMap<Key, Seen>,
    /// Counts the times anyone's presence changed, for lists that group by it.
    presence_rev: u64,
    /// Per conversation: who is typing, and when the shell heard.
    typing: HashMap<Key, (Vec<Typist>, i64)>,
    /// Per voice channel: the people in it.
    voice: HashMap<Key, Vec<Key>>,
    voice_order: VecDeque<Key>,
    people: HashMap<Key, Person>,
    people_order: VecDeque<Key>,
    files: HashMap<(AccountKey, String), Fetched>,
    file_order: VecDeque<(AccountKey, String)>,
    wanted_files: Vec<(AccountKey, String)>,
    fetch_pending: bool,
    /// Repaints the window when something a view looks up has changed.
    notify: Option<Notify>,
}

/// The shared handle. Cheap to clone; every view that draws people holds one.
#[derive(Clone, Default)]
pub struct Hub(Rc<RefCell<Inner>>);

impl Hub {
    pub fn on_change(&self, notify: impl Fn(&mut App) + 'static) {
        self.0.borrow_mut().notify = Some(Rc::new(notify));
    }

    pub fn notify(&self, cx: &mut App) {
        let notify = self.0.borrow().notify.clone();
        if let Some(notify) = notify {
            notify(cx);
        }
    }

    /// The people `source` draws now. Call it whenever the visible rows change, with every
    /// person on screen; an empty list when the view goes away.
    pub fn watch(&self, source: &'static str, people: impl IntoIterator<Item = Key>, cx: &mut App) {
        let people = people.into_iter().collect();
        if self.0.borrow_mut().watch.people(source, people, Instant::now()) {
            self.schedule(cx);
        }
    }

    /// The conversation rows `source` draws now, so chatd fills in their previews. An empty
    /// list when the view goes away.
    pub fn watch_rows(&self, source: &'static str, rows: impl IntoIterator<Item = Key>, cx: &mut App) {
        let rows = rows.into_iter().collect();
        if self.0.borrow_mut().watch.rows(source, rows, Instant::now()) {
            self.schedule(cx);
        }
    }

    /// Whether the window has focus, for chatd's notifications.
    pub fn watch_focus(&self, focused: bool, cx: &mut App) {
        if self.0.borrow_mut().watch.focused(focused, Instant::now()) {
            self.schedule(cx);
        }
    }

    /// The open conversation, for its typing and member list.
    pub fn watch_conversation(&self, conversation: Option<&Key>, cx: &mut App) {
        if self.0.borrow_mut().watch.conversation(conversation.cloned(), Instant::now()) {
            self.schedule(cx);
        }
    }

    pub fn watch_members(&self, rows: Option<(u32, u32)>, cx: &mut App) {
        if self.0.borrow_mut().watch.members(rows, Instant::now()) {
            self.schedule(cx);
        }
    }

    /// The Space whose tree the sidebar shows, so its live subscription is kept.
    pub fn watch_space(&self, space: Option<&Key>, cx: &mut App) {
        if self.0.borrow_mut().watch.space(space.cloned(), Instant::now()) {
            self.schedule(cx);
        }
    }

    /// Sends `Watch` once the set has settled. One timer at most, and none while nothing changes.
    fn schedule(&self, cx: &mut App) {
        if std::mem::replace(&mut self.0.borrow_mut().watch_pending, true) {
            return;
        }
        let hub = self.clone();
        cx.spawn(async move |cx| {
            loop {
                let due = hub.0.borrow_mut().watch.due(Instant::now());
                match due {
                    Due::Wait(wait) => cx.background_executor().timer(wait).await,
                    Due::Nothing => break,
                    Due::Send(watching) => {
                        let request = Request::Watch(watching);
                        let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
                        match answer {
                            Ok(Response::Watching { people, typists }) => {
                                cx.update(|cx| hub.watching(people, typists, cx));
                            }
                            // chatd is away. The shell reads everything again when it is back,
                            // and that is when this is sent again.
                            _ => hub.0.borrow_mut().watch.sent = None,
                        }
                    }
                }
            }
            hub.0.borrow_mut().watch_pending = false;
        })
        .detach();
    }

    fn watching(&self, people: Vec<Person>, typists: Vec<Typist>, cx: &mut App) {
        {
            let mut inner = self.0.borrow_mut();
            // What is no longer on screen is no longer kept current, so it is not kept.
            let Inner { watch, presence, .. } = &mut *inner;
            presence.retain(|person, _| watch.is_watching(person));
            if let Some(open) = watch.conversation.clone() {
                inner.typing.insert(open, (typists, now()));
            }
        }
        self.learn(&people);
        self.notify(cx);
    }

    /// Takes the ephemeral changes; the stored kinds are for whoever holds the list.
    pub fn changed(&self, change: &Change, cx: &mut App) {
        match change {
            Change::Presence { person, presence, status, .. } => {
                self.set_presence(person, *presence, status);
                self.notify(cx);
            }
            Change::Typing { conversation, typists } => {
                let mut inner = self.0.borrow_mut();
                if typists.is_empty() {
                    inner.typing.remove(conversation);
                } else {
                    inner.typing.insert(conversation.clone(), (typists.clone(), now()));
                }
                // Other conversations' lines have had their time by now.
                let now = now();
                inner.typing.retain(|_, (typists, heard)| live(typists, *heard, now).next().is_some());
                drop(inner);
                self.notify(cx);
            }
            Change::Occupants { conversation, occupants } => {
                if self.set_occupants(conversation, occupants) {
                    self.notify(cx);
                }
            }
            Change::Media { account, file } => {
                let fetched = Fetched::of(file, Instant::now()).unwrap_or(Fetched::Failed(Instant::now()));
                self.0.borrow_mut().files.insert((account.clone(), file.id.clone()), fetched);
                self.notify(cx);
            }
            // The links may have dropped and come back, and chatd may have restarted.
            Change::Status => {
                self.0.borrow_mut().watch.forget(Instant::now());
                self.schedule(cx);
            }
            _ => {}
        }
    }

    fn set_presence(&self, person: &Key, presence: Presence, status: &str) {
        let mut inner = self.0.borrow_mut();
        if inner.presence.len() >= PRESENCE_LIMIT && !inner.presence.contains_key(person) {
            let Inner { watch, presence, .. } = &mut *inner;
            presence.retain(|person, _| watch.is_watching(person));
            if presence.len() >= PRESENCE_LIMIT {
                presence.clear();
            }
        }
        let seen = Seen { presence, status: status.to_owned() };
        if inner.presence.insert(person.clone(), seen).is_none_or(|before| before.presence != presence) {
            inner.presence_rev += 1;
        }
    }

    /// Who is in a voice channel now, from a change or from a row just read. True when that
    /// changed who is in voice.
    pub fn set_occupants(&self, conversation: &Key, occupants: &Occupants) -> bool {
        let mut inner = self.0.borrow_mut();
        let people: Vec<Key> = occupants.people.iter().map(|occupant| occupant.person.clone()).collect();
        if people.is_empty() {
            inner.voice_order.retain(|known| known != conversation);
            return inner.voice.remove(conversation).is_some();
        }
        if inner.voice.get(conversation) == Some(&people) {
            return false;
        }
        if inner.voice.insert(conversation.clone(), people).is_none() {
            inner.voice_order.push_back(conversation.clone());
            if inner.voice_order.len() > VOICE_LIMIT {
                if let Some(old) = inner.voice_order.pop_front() {
                    inner.voice.remove(&old);
                }
            }
        }
        true
    }

    /// Whether this person is in a voice channel we have heard about.
    pub fn in_voice(&self, person: &Key) -> bool {
        self.0.borrow().voice.values().flatten().any(|occupant| occupant == person)
    }

    pub fn presence_rev(&self) -> u64 {
        self.0.borrow().presence_rev
    }

    /// People a list or an answer just showed us: their presence as chatd had it, and who they
    /// are, for views that only hold a key.
    pub fn learn(&self, people: &[Person]) {
        for person in people {
            if person.presence != Presence::Unknown || !person.status.is_empty() {
                self.set_presence(&person.key, person.presence, &person.status);
            }
            let mut inner = self.0.borrow_mut();
            if inner.people.insert(person.key.clone(), person.clone()).is_none() {
                inner.people_order.push_back(person.key.clone());
                if inner.people_order.len() > PEOPLE_LIMIT {
                    if let Some(old) = inner.people_order.pop_front() {
                        inner.people.remove(&old);
                    }
                }
            }
        }
    }

    /// The network agreed to what the user did about a person: what is remembered of them
    /// follows, so a card opened next does not show the old relation.
    pub fn related(&self, person: &Key, action: RelateAction) {
        if let Some(known) = self.0.borrow_mut().people.get_mut(person) {
            known.relation = action.leaves(known.relation);
        }
    }

    pub fn person(&self, person: &Key) -> Option<Person> {
        self.0.borrow().people.get(person).cloned()
    }

    /// `Unknown` for anyone not watched: it draws as no dot at all.
    pub fn presence_of(&self, person: &Key) -> Presence {
        self.0.borrow().presence.get(person).map_or(Presence::Unknown, |seen| seen.presence)
    }

    pub fn status_of(&self, person: &Key) -> String {
        self.0.borrow().presence.get(person).map_or_else(String::new, |seen| seen.status.clone())
    }

    /// Who is typing in a conversation right now. Lines that have had their time are left out.
    pub fn typists_in(&self, conversation: &Key) -> Vec<Typist> {
        // M3 off: nobody is drawn typing, in the list or the conversation.
        if !crate::gui::app::prefs::choices().typing {
            return Vec::new();
        }
        let inner = self.0.borrow();
        let Some((typists, heard)) = inner.typing.get(conversation) else {
            return Vec::new();
        };
        live(typists, *heard, now()).cloned().collect()
    }

    /// Unix seconds at which the next typist of a conversation times out.
    pub fn typing_ends(&self, conversation: &Key) -> Option<i64> {
        let inner = self.0.borrow();
        let (typists, heard) = inner.typing.get(conversation)?;
        live(typists, *heard, now()).map(|typist| typist.until.min(heard + TYPING_SECONDS)).min()
    }

    /// The same file with its path, when it is on disk. When it is not, it is asked for, and the
    /// window repaints as it lands. Call it only for what is on screen.
    pub fn file(&self, account: &AccountKey, file: &MediaFile, cx: &mut App) -> MediaFile {
        if file.path.is_some() {
            return file.clone();
        }
        let id = (account.clone(), file.id.clone());
        let mut inner = self.0.borrow_mut();
        match inner.files.get(&id) {
            Some(Fetched::Here(path)) => {
                return MediaFile { path: Some(path.clone()), ..file.clone() };
            }
            Some(fetched) if !fetched.ask_again(Instant::now()) => return file.clone(),
            Some(_) | None => {}
        }
        if inner.files.insert(id.clone(), Fetched::Asked).is_none() {
            inner.file_order.push_back(id.clone());
        }
        if inner.file_order.len() > FILE_LIMIT {
            if let Some(old) = inner.file_order.pop_front() {
                inner.files.remove(&old);
            }
        }
        inner.wanted_files.push(id);
        if !std::mem::replace(&mut inner.fetch_pending, true) {
            let hub = self.clone();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FETCH_QUIET).await;
                cx.update(|cx| hub.fetch(cx));
            })
            .detach();
        }
        file.clone()
    }

    /// A person's or conversation's picture, ready for `Avatar::image`.
    pub fn avatar(&self, account: &AccountKey, file: Option<&MediaFile>, cx: &mut App) -> Option<MediaFile> {
        file.map(|file| self.file(account, file, cx))
    }

    fn fetch(&self, cx: &mut App) {
        let wanted = {
            let mut inner = self.0.borrow_mut();
            inner.fetch_pending = false;
            std::mem::take(&mut inner.wanted_files)
        };
        let mut by_account: Vec<(AccountKey, Vec<String>)> = Vec::new();
        for (account, id) in wanted {
            match by_account.iter_mut().find(|(known, _)| *known == account) {
                Some((_, ids)) => ids.push(id),
                None => by_account.push((account, vec![id])),
            }
        }
        for (account, ids) in by_account {
            let hub = self.clone();
            let request = Request::Fetch { retry: false, size: None, account: account.clone(), ids };
            ask_later(cx, request, move |answer, cx| {
                let Ok(Response::Media { files }) = answer else {
                    return;
                };
                let mut landed = false;
                let at = Instant::now();
                for file in files {
                    // Not arriving, for now: asked for again later rather than never.
                    let Some(fetched) = Fetched::of(&file, at) else {
                        continue;
                    };
                    landed |= matches!(fetched, Fetched::Here(_));
                    hub.0.borrow_mut().files.insert((account.clone(), file.id), fetched);
                }
                if landed {
                    hub.notify(cx);
                }
            });
        }
    }
}

/// What a person is to us once the network has agreed to an action.
pub trait Leaves {
    fn leaves(self, before: Relation) -> Relation;
}

impl Leaves for RelateAction {
    fn leaves(self, before: Relation) -> Relation {
        match self {
            RelateAction::Accept => Relation::Friend,
            RelateAction::Ignore | RelateAction::Cancel | RelateAction::Remove => match before {
                Relation::Blocked => Relation::Blocked,
                _ => Relation::None,
            },
            RelateAction::Block => Relation::Blocked,
        }
    }
}

fn live(typists: &[Typist], heard: i64, now: i64) -> impl Iterator<Item = &Typist> {
    typists.iter().filter(move |typist| now < typist.until.min(heard + TYPING_SECONDS))
}

#[cfg(test)]
mod tests {
    #[test]
    fn member_viewports_settle_within_a_frame_and_do_not_wait_for_presence_quiet() {
        let start = std::time::Instant::now();
        let mut watch = super::WatchSet::default();
        watch.members(Some((0, 50)), start);
        assert!(matches!(watch.due(start + std::time::Duration::from_millis(16)), super::Due::Send(_)));
        for step in 1..=5 {
            watch.members(Some((step * 50, 50)), start + std::time::Duration::from_millis(16 + step as u64 * 10));
        }
        let due = watch.due(start + std::time::Duration::from_millis(76));
        assert!(matches!(due, super::Due::Send(ref watching) if watching.members == Some((250, 50))));
    }

    use super::*;
    use protocol::Network;

    fn key(id: &str) -> Key {
        Key { network: Network::Discord, account: "1".into(), id: id.into() }
    }

    fn keys(ids: &[&str]) -> Vec<Key> {
        ids.iter().map(|id| key(id)).collect()
    }

    #[test]
    fn a_burst_of_changes_is_one_watch_after_it_settles() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut watch = WatchSet::default();
        assert_eq!(watch.due(at(0)), Due::Nothing);

        assert!(watch.people("sidebar", keys(&["a", "b"]), at(0)));
        assert!(watch.conversation(Some(key("c1")), at(50)));
        assert!(watch.people("people", keys(&["b", "c"]), at(120)));
        assert_eq!(watch.due(at(130)), Due::Wait(Duration::from_millis(190)));
        let Due::Send(sent) = watch.due(at(320)) else {
            panic!("the set has been still for the quiet time");
        };
        assert_eq!(sent.conversation, Some(key("c1")));
        // Each person once, however many views draw them.
        assert_eq!(sent.people.len(), 3);
        assert_eq!(watch.due(at(400)), Due::Nothing);
    }

    #[test]
    fn drawing_the_same_people_again_sends_nothing() {
        let start = Instant::now();
        let mut watch = WatchSet::default();
        watch.people("sidebar", keys(&["a", "b"]), start);
        assert!(matches!(watch.due(start + QUIET), Due::Send(_)));
        assert!(!watch.people("sidebar", keys(&["a", "b"]), start + QUIET * 2));
        assert_eq!(watch.due(start + QUIET * 4), Due::Nothing);
        // A second view showing someone already watched changes nothing chatd knows.
        assert!(watch.people("members", keys(&["b"]), start + QUIET * 5));
        assert_eq!(watch.due(start + QUIET * 7), Due::Nothing);
        // And going back to what was sent before the debounce ran sends nothing either.
        watch.people("sidebar", keys(&["a"]), start + QUIET * 8);
        watch.people("sidebar", keys(&["a", "b"]), start + QUIET * 8);
        assert_eq!(watch.due(start + QUIET * 10), Due::Nothing);
    }

    #[test]
    fn a_scroll_that_never_stops_is_still_sent_within_a_second() {
        let start = Instant::now();
        let mut watch = WatchSet::default();
        let mut sends = 0;
        for step in 0..30u32 {
            let now = start + Duration::from_millis(50) * step;
            watch.people("people", keys(&[&format!("p{step}")]), now);
            if matches!(watch.due(now), Due::Send(_)) {
                sends += 1;
            }
        }
        assert_eq!(sends, 1);
    }

    #[test]
    fn a_view_going_away_and_a_closed_conversation_are_told_too() {
        let start = Instant::now();
        let mut watch = WatchSet::default();
        watch.people("people", keys(&["a"]), start);
        watch.conversation(Some(key("c1")), start);
        watch.members(Some((0, 40)), start);
        assert!(matches!(watch.due(start + LATEST), Due::Send(_)));
        assert!(watch.people("people", Vec::new(), start + LATEST));
        assert!(!watch.people("people", Vec::new(), start + LATEST));
        // Another conversation has another member list.
        assert!(watch.conversation(None, start + LATEST));
        assert_eq!(watch.due(start + LATEST * 2), Due::Send(Watching::default()));
        assert!(!watch.is_watching(&key("a")));
    }

    #[test]
    fn the_watch_is_capped_at_what_chatd_accepts() {
        let start = Instant::now();
        let mut watch = WatchSet::default();
        let many: Vec<Key> = (0..WATCH_LIMIT + 50).map(|n| key(&n.to_string())).collect();
        watch.people("people", many, start);
        let Due::Send(sent) = watch.due(start + LATEST) else {
            panic!("due");
        };
        assert_eq!(sent.people.len(), WATCH_LIMIT);
    }

    #[test]
    fn after_chatd_restarts_the_same_set_is_sent_again() {
        let start = Instant::now();
        let mut watch = WatchSet::default();
        watch.people("people", keys(&["a"]), start);
        assert!(matches!(watch.due(start + LATEST), Due::Send(_)));
        watch.forget(start + LATEST);
        assert!(matches!(watch.due(start + LATEST * 2), Due::Send(_)));
        assert_eq!(watch.due(start + LATEST * 3), Due::Nothing);
    }

    #[test]
    fn the_space_on_show_is_part_of_what_is_watched() {
        let start = Instant::now();
        let mut watch = WatchSet::default();
        assert!(watch.space(Some(key("server")), start));
        assert!(!watch.space(Some(key("server")), start));
        let Due::Send(sent) = watch.due(start + LATEST) else {
            panic!("due");
        };
        assert_eq!(sent.space, Some(key("server")));
        // Leaving the Space is told too, and going back to what was sent is not.
        assert!(watch.space(None, start + LATEST));
        assert_eq!(watch.due(start + LATEST * 2), Due::Send(Watching::default()));
        watch.space(Some(key("other")), start + LATEST * 2);
        watch.space(None, start + LATEST * 2);
        assert_eq!(watch.due(start + LATEST * 3), Due::Nothing);
    }

    #[test]
    fn who_is_in_voice_follows_each_channel_and_is_bounded() {
        let hub = Hub::default();
        let occupants = |ids: &[&str]| Occupants {
            people: ids
                .iter()
                .map(|id| protocol::Occupant { person: key(id), name: (*id).into(), muted: false, deafened: false, streaming: false, speaking: false })
                .collect(),
            total: ids.len() as u32,
        };
        assert!(hub.set_occupants(&key("voice"), &occupants(&["ines", "kofi"])));
        assert!(!hub.set_occupants(&key("voice"), &occupants(&["ines", "kofi"])));
        assert!(hub.in_voice(&key("ines")) && !hub.in_voice(&key("mara")));
        assert!(hub.set_occupants(&key("voice"), &occupants(&["kofi"])));
        assert!(!hub.in_voice(&key("ines")));
        assert!(hub.set_occupants(&key("voice"), &Occupants::default()));
        assert!(!hub.in_voice(&key("kofi")));
        assert!(!hub.set_occupants(&key("voice"), &Occupants::default()));

        for channel in 0..VOICE_LIMIT + 10 {
            hub.set_occupants(&key(&format!("v{channel}")), &occupants(&[&format!("p{channel}")]));
        }
        assert_eq!(hub.0.borrow().voice.len(), VOICE_LIMIT);
        assert!(!hub.in_voice(&key("p0")) && hub.in_voice(&key(&format!("p{}", VOICE_LIMIT + 9))));
    }

    #[test]
    fn an_action_the_network_agreed_to_changes_what_the_person_is() {
        use RelateAction::{Accept, Block, Cancel, Ignore, Remove};
        assert_eq!(Accept.leaves(Relation::Incoming), Relation::Friend);
        assert_eq!(Ignore.leaves(Relation::Incoming), Relation::None);
        assert_eq!(Cancel.leaves(Relation::Outgoing), Relation::None);
        assert_eq!(Remove.leaves(Relation::Friend), Relation::None);
        assert_eq!(Block.leaves(Relation::Friend), Relation::Blocked);
    }

    #[test]
    fn a_typist_is_dropped_at_until_or_twelve_seconds_after_hearing() {
        let typist = |name: &str, until: i64| Typist { id: name.into(), name: name.into(), until };
        let typists = [typist("ines", 105), typist("tomasz", 200)];
        let names = |now: i64| live(&typists, 100, now).map(|typist| typist.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(104), ["ines", "tomasz"]);
        assert_eq!(names(105), ["tomasz"]);
        assert_eq!(names(111), ["tomasz"]);
        assert!(names(112).is_empty());
    }

    #[test]
    fn a_picture_that_did_not_arrive_is_asked_for_again_after_a_while() {
        let at = Instant::now();
        let mut file = MediaFile::new("a", protocol::MediaKind::Avatar);
        // Queued: it lands as a change, nothing to note yet.
        assert!(Fetched::of(&file, at).is_none());
        file.failed = Some("The download failed: tls: invalid peer certificate.".into());
        let failed = Fetched::of(&file, at).unwrap();
        assert!(!failed.ask_again(at));
        assert!(!failed.ask_again(at + FILE_AGAIN / 2));
        assert!(failed.ask_again(at + FILE_AGAIN));
        // What landed, or is on its way, is never asked for twice.
        assert!(!Fetched::Asked.ask_again(at + FILE_AGAIN * 10));
        file.path = Some("/media/a".into());
        assert!(matches!(Fetched::of(&file, at), Some(Fetched::Here(_))));
    }
}
