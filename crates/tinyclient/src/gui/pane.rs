//! The right pane: one frame whose kind the user picked and whose subject follows the open
//! conversation (`product/polish/07` §1, ruling 2).
//!
//! Details is sticky and remembered as two switches, one for channels and groups and one for
//! direct conversations; its subject is always derived from what is open. A thread belongs to
//! one conversation: on every switch it is put away for that conversation and comes back on
//! return, so its composer can never sit beside a conversation it does not post to. Search
//! never follows. A layer opened from inside the pane is pushed over the base, at most two deep,
//! and dropped on a switch.
//!
//! Everything here is plain data and rules; `conversation.rs` applies them to the window.

use protocol::{Key, Message};
use std::collections::VecDeque;
use std::rc::Rc;

/// Every kind is this wide; the user drags it within [`RANGE`].
// CONTRACT: `layout.rs` still sizes Members at `MEMBERS_WIDTH` 240 and the rest at
// `RIGHT_WIDTH` 340; both should become `pane::WIDTH`, clamped to `pane::RANGE`.
#[allow(dead_code)]
pub const WIDTH: f32 = 300.;
#[allow(dead_code)]
pub const RANGE: (f32, f32) = (260., 420.);
/// Conversations whose thread and scroll place are remembered, most recent first.
pub const REMEMBERED: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub enum Layer {
    /// One person: the other side of a direct conversation, or someone picked from a list.
    Profile(Key),
    /// What a group or channel is.
    Details(Key),
    Members(Key),
    /// A thread of `conversation`, by its root.
    Thread { conversation: Key, root: Rc<Message> },
    /// The messages pinned in a conversation.
    Pinned(Key),
    /// A Space's own details, opened from its name in the sidebar.
    Space(Key),
    /// Search results, which never follow.
    #[allow(dead_code)]
    Search,
}

impl Layer {
    pub fn title(&self) -> &'static str {
        match self {
            Layer::Profile(_) => "Profile",
            Layer::Details(_) => "Details",
            Layer::Members(_) => "Members",
            Layer::Thread { .. } => "Thread",
            Layer::Pinned(_) => "Pinned",
            Layer::Space(_) => "Details",
            Layer::Search => "Search",
        }
    }

    /// The conversation this layer is about, when it is about one.
    pub fn conversation(&self) -> Option<&Key> {
        match self {
            Layer::Details(key) | Layer::Members(key) | Layer::Pinned(key) => Some(key),
            Layer::Thread { conversation, .. } => Some(conversation),
            Layer::Profile(_) | Layer::Space(_) | Layer::Search => None,
        }
    }

    /// Details in any of its shapes: what the switches remember, and what Escape never closes.
    pub fn is_details(&self) -> bool {
        matches!(self, Layer::Profile(_) | Layer::Details(_) | Layer::Members(_))
    }
}

/// The channel switch: what Details shows beside a channel or group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelSwitch {
    #[default]
    Off,
    Members,
    Details,
}

/// The two Details switches. They belong to the window and survive a restart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Switches {
    pub channel: ChannelSwitch,
    pub direct: bool,
}

/// What the pane holds: a base, and at most one layer pushed over it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    pub base: Option<Layer>,
    pub push: Option<Layer>,
}

impl Stack {
    pub fn top(&self) -> Option<&Layer> {
        self.push.as_ref().or(self.base.as_ref())
    }

    pub fn layers(&self) -> impl Iterator<Item = &Layer> {
        self.base.iter().chain(&self.push)
    }

    pub fn thread(&self) -> Option<(&Key, &Rc<Message>)> {
        self.layers().find_map(|layer| match layer {
            Layer::Thread { conversation, root } => Some((conversation, root)),
            _ => None,
        })
    }
}

/// The conversation now open, as far as the pane needs it.
#[derive(Clone, Debug)]
pub struct Open {
    pub key: Key,
    /// A direct conversation: the other person, when known.
    pub direct: Option<Option<Key>>,
}

/// What the Details switches show beside `open`.
pub fn derived(switches: Switches, open: &Open) -> Option<Layer> {
    match &open.direct {
        Some(peer) => peer.clone().filter(|_| switches.direct).map(Layer::Profile),
        None => match switches.channel {
            ChannelSwitch::Off => None,
            ChannelSwitch::Members => Some(Layer::Members(open.key.clone())),
            ChannelSwitch::Details => Some(Layer::Details(open.key.clone())),
        },
    }
}

/// A small most-recently-used map, by conversation.
#[derive(Debug)]
pub struct Memory<T> {
    entries: VecDeque<(Key, T)>,
    limit: usize,
}

impl<T> Default for Memory<T> {
    fn default() -> Self {
        Memory { entries: VecDeque::new(), limit: REMEMBERED }
    }
}

impl<T> Memory<T> {
    pub fn get(&self, key: &Key) -> Option<&T> {
        self.entries.iter().find(|(held, _)| held == key).map(|(_, value)| value)
    }

    pub fn put(&mut self, key: Key, value: T) {
        self.forget(&key);
        self.entries.push_front((key, value));
        self.entries.truncate(self.limit);
    }

    pub fn take(&mut self, key: &Key) -> Option<T> {
        let at = self.entries.iter().position(|(held, _)| held == key)?;
        self.entries.remove(at).map(|(_, value)| value)
    }

    pub fn forget(&mut self, key: &Key) {
        self.entries.retain(|(held, _)| held != key);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The pane after a change of conversation (07 §1). A thread is put away for the conversation
/// it belongs to; the one coming in gets its own remembered thread back, pushed over what the
/// switches say so its crumb still leads there, or just what the switches say. Search stays as
/// it was. With nothing open the pane closes, and a thread of the conversation that went is
/// forgotten. `replace`: the window is too narrow for the pane beside the conversation, so a
/// remembered thread stays put away until asked for and the conversation shows (07 §4).
pub fn follow(stack: &Stack, switches: Switches, to: Option<&Open>, threads: &mut Memory<Rc<Message>>, replace: bool) -> Stack {
    if matches!(stack.base, Some(Layer::Search)) {
        return Stack { base: Some(Layer::Search), push: None };
    }
    if let Some((conversation, root)) = stack.thread() {
        match to {
            Some(_) => threads.put(conversation.clone(), root.clone()),
            None => threads.forget(conversation),
        }
    }
    let Some(to) = to else {
        return Stack::default();
    };
    let details = derived(switches, to);
    let thread = if replace { None } else { threads.take(&to.key) };
    match thread.map(|root| Layer::Thread { conversation: to.key.clone(), root }) {
        Some(thread) if details.is_some() => Stack { base: details, push: Some(thread) },
        Some(thread) => Stack { base: Some(thread), push: None },
        None => Stack { base: details, push: None },
    }
}

/// Keeps a thread that `from` held and `to` does not, so it comes back with its conversation:
/// search opened over it, or anything else that took its place without a switch.
pub fn keep_thread(from: &Stack, to: &Stack, threads: &mut Memory<Rc<Message>>) {
    if let Some((conversation, root)) = from.thread() {
        if to.thread().is_none() {
            threads.put(conversation.clone(), root.clone());
        }
    }
}

/// The header's Details button: beside a direct conversation the other person, beside a
/// channel or group its details. Lit means on screen, so a click on a lit one turns it off.
pub fn toggle_details(stack: &Stack, switches: Switches, open: &Open) -> (Stack, Switches) {
    let mut switches = switches;
    let showing = stack.base.as_ref().is_some_and(|base| {
        matches!((&open.direct, base), (Some(_), Layer::Profile(_)) | (None, Layer::Details(_)))
    }) && stack.push.is_none();
    if open.direct.is_some() {
        switches.direct = !showing;
    } else {
        switches.channel = if showing { ChannelSwitch::Off } else { ChannelSwitch::Details };
    }
    let base = if showing { None } else { derived(switches, open) };
    (Stack { base, push: None }, switches)
}

/// The header's Members button: the other value of the channel switch.
pub fn toggle_members(stack: &Stack, switches: Switches, open: &Open) -> (Stack, Switches) {
    let mut switches = switches;
    let showing = matches!(stack.base, Some(Layer::Members(_))) && stack.push.is_none();
    switches.channel = if showing { ChannelSwitch::Off } else { ChannelSwitch::Members };
    let base = if showing { None } else { derived(switches, open) };
    (Stack { base, push: None }, switches)
}

/// Opens a layer from inside the pane or from the conversation: over sticky Details it is
/// pushed, so closing it returns to Details; otherwise it is the base.
pub fn open(stack: &Stack, layer: Layer) -> Stack {
    match &stack.base {
        // Search is how you move around: always the base.
        _ if layer == Layer::Search => Stack { base: Some(layer), push: None },
        Some(base) if base.is_details() && *base != layer => Stack { base: Some(base.clone()), push: Some(layer) },
        // A thread or pins with someone pushed over them: the person replaces the push.
        Some(base @ (Layer::Thread { .. } | Layer::Pinned(_))) if matches!(layer, Layer::Profile(_)) => {
            Stack { base: Some(base.clone()), push: Some(layer) }
        }
        _ => Stack { base: Some(layer), push: None },
    }
}

/// Sticky Details beside a conversation in a window too narrow for both: it hides, its switch
/// stays on, and it comes back when there is room (07 §4). `asked`: the user opened it at this
/// width, as a one-off that replaces the conversation. Anything pushed over it still shows.
pub fn tucked(stack: &Stack, narrow: bool, asked: bool) -> bool {
    narrow && !asked && stack.push.is_none() && stack.base.as_ref().is_some_and(Layer::is_details)
}

/// The pane's ✕ or `cmd-.`: everything in it goes. Closing Details turns its switch off, or
/// the next conversation would open it again.
pub fn close(stack: &Stack, switches: Switches) -> Switches {
    let mut switches = switches;
    match stack.base {
        Some(Layer::Profile(_)) => switches.direct = false,
        Some(Layer::Members(_) | Layer::Details(_)) => switches.channel = ChannelSwitch::Off,
        _ => {}
    }
    switches
}

/// What is on screen, for the Escape ladder (07 §6).
#[derive(Clone, Copy, Debug, Default)]
pub struct Showing {
    pub autocomplete: bool,
    pub menu: bool,
    pub card: bool,
    pub viewer: bool,
    pub switcher: bool,
    pub floating_sidebar: bool,
    pub editing: bool,
    pub replying: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Autocomplete,
    Menu,
    Card,
    Viewer,
    Switcher,
    FloatingSidebar,
    Edit,
    Reply,
    /// The pushed layer goes; the base shows again.
    Pop,
    /// A thread, pins or search: the pane closes.
    ClosePane,
    /// Nothing else is open: read to the newest message, jump there, focus the composer.
    Latest,
}

/// The one thing Escape removes: the first that applies, top first. It never clears what was
/// typed and never closes Details.
pub fn escape(showing: Showing, stack: &Stack) -> Step {
    let ladder = [
        (showing.autocomplete, Step::Autocomplete),
        (showing.menu, Step::Menu),
        (showing.card, Step::Card),
        (showing.viewer, Step::Viewer),
        (showing.switcher, Step::Switcher),
        (showing.floating_sidebar, Step::FloatingSidebar),
        (showing.editing, Step::Edit),
        (showing.replying, Step::Reply),
        (stack.push.is_some(), Step::Pop),
        (stack.base.as_ref().is_some_and(|base| !base.is_details()), Step::ClosePane),
    ];
    ladder.into_iter().find(|(on, _)| *on).map_or(Step::Latest, |(_, step)| step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Network};

    fn key(id: &str) -> Key {
        Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, id)
    }

    fn channel(id: &str) -> Open {
        Open { key: key(id), direct: None }
    }

    fn direct(id: &str, peer: &str) -> Open {
        Open { key: key(id), direct: Some(Some(key(peer))) }
    }

    fn root(id: &str) -> Rc<Message> {
        Rc::new(Message::plain(id, "Root", "a root", 0, false))
    }

    fn thread(conversation: &str, id: &str) -> Layer {
        Layer::Thread { conversation: key(conversation), root: root(id) }
    }

    fn base(layer: Layer) -> Stack {
        Stack { base: Some(layer), push: None }
    }

    #[test]
    fn a_thread_never_stays_beside_another_conversation_and_returns_with_its_own() {
        // P0: a thread from #a beside #b would post to #a.
        let mut threads = Memory::default();
        let switches = Switches { channel: ChannelSwitch::Members, direct: false };
        let in_a = base(thread("C_a", "r1"));
        let in_b = follow(&in_a, switches, Some(&channel("C_b")), &mut threads, false);
        assert!(in_b.thread().is_none(), "the thread of #a must not be beside #b");
        assert_eq!(in_b, base(Layer::Members(key("C_b"))));
        // Into a DM with the DM switch off, nothing; still no thread.
        let in_dm = follow(&in_b, switches, Some(&direct("D_1", "U1")), &mut threads, false);
        assert_eq!(in_dm, Stack::default());
        // Back in #a, the same root.
        let back = follow(&in_dm, switches, Some(&channel("C_a")), &mut threads, false);
        assert_eq!(back.thread().map(|(conversation, root)| (conversation.clone(), root.id.clone())), Some((key("C_a"), "r1".into())));
        // A thread pushed over Details is put away just the same.
        let pushed = Stack { base: Some(Layer::Members(key("C_a"))), push: Some(thread("C_a", "r2")) };
        let away = follow(&pushed, switches, Some(&channel("C_c")), &mut threads, false);
        assert!(away.thread().is_none());
        assert_eq!(threads.get(&key("C_a")).map(|root| root.id.as_str()), Some("r2"));
    }

    #[test]
    fn the_pane_follows_the_table() {
        let mut threads = Memory::default();
        let both = Switches { channel: ChannelSwitch::Members, direct: true };
        // DM A's profile → DM B: the new partner.
        let a = base(Layer::Profile(key("U1")));
        assert_eq!(follow(&a, both, Some(&direct("D_b", "U2")), &mut threads, false), base(Layer::Profile(key("U2"))));
        // Profile → channel: the channel switch decides.
        assert_eq!(follow(&a, both, Some(&channel("C_1")), &mut threads, false), base(Layer::Members(key("C_1"))));
        let details = Switches { channel: ChannelSwitch::Details, direct: true };
        assert_eq!(follow(&a, details, Some(&channel("C_1")), &mut threads, false), base(Layer::Details(key("C_1"))));
        // Members → DM with the DM switch off: closed, and the channel switch stays on.
        let members_only = Switches { channel: ChannelSwitch::Members, direct: false };
        let members = base(Layer::Members(key("C_1")));
        assert_eq!(follow(&members, members_only, Some(&direct("D_c", "U3")), &mut threads, false), Stack::default());
        // Members → another channel: its members.
        assert_eq!(follow(&members, members_only, Some(&channel("C_2")), &mut threads, false), base(Layer::Members(key("C_2"))));
        // A pushed profile is dropped; the base follows its own rule.
        let pushed = Stack { base: Some(Layer::Members(key("C_1"))), push: Some(Layer::Profile(key("U4"))) };
        assert_eq!(follow(&pushed, members_only, Some(&channel("C_2")), &mut threads, false), base(Layer::Members(key("C_2"))));
        // Search stays wherever the user goes.
        let search = base(Layer::Search);
        assert_eq!(follow(&search, both, Some(&channel("C_2")), &mut threads, false), search);
        // Nothing open: closed, and a thread of the conversation that went is forgotten.
        let gone = base(thread("C_x", "r9"));
        assert_eq!(follow(&gone, both, None, &mut threads, false), Stack::default());
        assert!(threads.get(&key("C_x")).is_none());
        // A DM whose partner is not known yet shows nothing rather than someone else.
        let unknown = Open { key: key("D_d"), direct: Some(None) };
        assert_eq!(follow(&a, both, Some(&unknown), &mut threads, false), Stack::default());
    }

    #[test]
    fn the_two_switches_are_separate_and_a_lit_button_turns_its_switch_off() {
        let off = Switches::default();
        let (stack, switches) = toggle_details(&Stack::default(), off, &direct("D_1", "U1"));
        assert_eq!((stack.clone(), switches), (base(Layer::Profile(key("U1"))), Switches { channel: ChannelSwitch::Off, direct: true }));
        let (stack, switches) = toggle_details(&stack, switches, &direct("D_1", "U1"));
        assert_eq!((stack, switches.direct), (Stack::default(), false));

        let (stack, switches) = toggle_members(&Stack::default(), off, &channel("C_1"));
        assert_eq!(stack, base(Layer::Members(key("C_1"))));
        // Details is the switch's other value: one click moves it, members go.
        let (stack, switches) = toggle_details(&stack, switches, &channel("C_1"));
        assert_eq!((stack.clone(), switches.channel), (base(Layer::Details(key("C_1"))), ChannelSwitch::Details));
        let (stack, switches) = toggle_details(&stack, switches, &channel("C_1"));
        assert_eq!((stack, switches.channel), (Stack::default(), ChannelSwitch::Off));
        // Closing with ✕ turns the shown kind's switch off, and only that one.
        let both = Switches { channel: ChannelSwitch::Members, direct: true };
        assert_eq!(close(&base(Layer::Members(key("C_1"))), both), Switches { channel: ChannelSwitch::Off, direct: true });
        assert_eq!(close(&base(thread("C_1", "r1")), both), both);
    }

    #[test]
    fn layers_opened_over_details_are_pushed_at_most_two_deep() {
        let members = base(Layer::Members(key("C_1")));
        let with_thread = open(&members, thread("C_1", "r1"));
        assert_eq!(with_thread.base, members.base);
        assert!(matches!(with_thread.push, Some(Layer::Thread { .. })));
        // Another layer replaces the push; it never goes three deep.
        let person = open(&with_thread, Layer::Profile(key("U1")));
        assert_eq!((person.base, person.push), (members.base.clone(), Some(Layer::Profile(key("U1")))));
        // A person from a thread goes over the thread.
        let over = open(&base(thread("C_1", "r1")), Layer::Profile(key("U2")));
        assert!(matches!(over.base, Some(Layer::Thread { .. })) && over.push == Some(Layer::Profile(key("U2"))));
        // With nothing sticky, it is the base.
        assert_eq!(open(&Stack::default(), Layer::Pinned(key("C_1"))), base(Layer::Pinned(key("C_1"))));
    }

    #[test]
    fn escape_takes_one_thing_top_first_and_never_details() {
        let all = Showing {
            autocomplete: true,
            menu: true,
            card: true,
            viewer: true,
            switcher: true,
            floating_sidebar: true,
            editing: true,
            replying: true,
        };
        let pushed = Stack { base: Some(Layer::Members(key("C_1"))), push: Some(thread("C_1", "r1")) };
        // Peel the layers off one at a time, as presses would.
        let mut showing = all;
        let mut order = Vec::new();
        let mut stack = pushed;
        loop {
            let step = escape(showing, &stack);
            order.push(step);
            match step {
                Step::Autocomplete => showing.autocomplete = false,
                Step::Menu => showing.menu = false,
                Step::Card => showing.card = false,
                Step::Viewer => showing.viewer = false,
                Step::Switcher => showing.switcher = false,
                Step::FloatingSidebar => showing.floating_sidebar = false,
                Step::Edit => showing.editing = false,
                Step::Reply => showing.replying = false,
                Step::Pop => stack.push = None,
                Step::ClosePane => stack.base = None,
                Step::Latest => break,
            }
        }
        use Step::*;
        assert_eq!(order, [Autocomplete, Menu, Card, Viewer, Switcher, FloatingSidebar, Edit, Reply, Pop, Latest]);
        // Members stayed: Details never closes on Escape.
        assert_eq!(stack.base, Some(Layer::Members(key("C_1"))));
        // A thread as the base closes.
        assert_eq!(escape(Showing::default(), &base(thread("C_1", "r1"))), ClosePane);
    }

    #[test]
    fn a_thread_comes_back_over_members_with_its_crumb_and_stays_away_in_a_narrow_window() {
        let mut threads = Memory::default();
        let members = Switches { channel: ChannelSwitch::Members, direct: false };
        // A thread opened over Members in #a, then #b, then back.
        let over = open(&base(Layer::Members(key("C_a"))), thread("C_a", "r1"));
        let in_b = follow(&over, members, Some(&channel("C_b")), &mut threads, false);
        assert_eq!(in_b, base(Layer::Members(key("C_b"))));
        let back = follow(&in_b, members, Some(&channel("C_a")), &mut threads, false);
        assert_eq!(back.base, Some(Layer::Members(key("C_a"))), "the crumb still leads to Members");
        assert!(matches!(&back.push, Some(Layer::Thread { root, .. }) if root.id == "r1"));
        // With nothing sticky the thread is the base again.
        let off = Switches::default();
        let away = follow(&base(thread("C_a", "r2")), off, Some(&channel("C_b")), &mut threads, false);
        assert_eq!(away, Stack::default());
        assert!(matches!(follow(&away, off, Some(&channel("C_a")), &mut threads, false).base, Some(Layer::Thread { .. })));
        // Narrow: returning shows the conversation; the thread stays remembered until asked.
        let away = follow(&base(thread("C_a", "r3")), off, Some(&channel("C_b")), &mut threads, true);
        let back = follow(&away, off, Some(&channel("C_a")), &mut threads, true);
        assert_eq!(back, Stack::default());
        assert_eq!(threads.get(&key("C_a")).map(|root| root.id.as_str()), Some("r3"));
        // Search taking the thread's place keeps it; search itself is always a base.
        let mut threads = Memory::default();
        let thread_open = base(thread("C_a", "r4"));
        let search = open(&thread_open, Layer::Search);
        keep_thread(&thread_open, &search, &mut threads);
        assert_eq!(search, base(Layer::Search));
        assert_eq!(threads.get(&key("C_a")).map(|root| root.id.as_str()), Some("r4"));
        assert_eq!(open(&base(Layer::Members(key("C_a"))), Layer::Search), base(Layer::Search));
    }

    #[test]
    fn details_hides_in_a_narrow_window_unless_asked_and_a_push_still_shows() {
        let members = base(Layer::Members(key("C_1")));
        assert!(!tucked(&members, false, false), "room beside: shown");
        assert!(tucked(&members, true, false), "narrow: hidden, the conversation shows");
        assert!(!tucked(&members, true, true), "asked for at this width: a one-off");
        assert!(tucked(&base(Layer::Profile(key("U1"))), true, false));
        // A thread or a pushed layer replaces the conversation instead.
        assert!(!tucked(&base(thread("C_1", "r1")), true, false));
        assert!(!tucked(&open(&members, thread("C_1", "r1")), true, false));
        assert!(!tucked(&Stack::default(), true, false));
    }

    #[test]
    fn escape_with_the_composer_focused_closes_the_thread_first_time() {
        // The composer handles its own list, edit and reply; with none of them up, what it hands
        // on is this, and one press closes the thread.
        let thread_open = base(thread("C_1", "r1"));
        assert_eq!(escape(Showing::default(), &thread_open), Step::ClosePane);
        // Reply mode comes first, then the thread.
        assert_eq!(escape(Showing { replying: true, ..Showing::default() }, &thread_open), Step::Reply);
        // Members never closes: the step is "to the newest".
        assert_eq!(escape(Showing::default(), &base(Layer::Members(key("C_1")))), Step::Latest);
        // An edit or the image viewer is someone else's step: Details stays.
        assert_eq!(escape(Showing { editing: true, ..Showing::default() }, &base(Layer::Details(key("C_1")))), Step::Edit);
        assert_eq!(escape(Showing { viewer: true, ..Showing::default() }, &base(Layer::Details(key("C_1")))), Step::Viewer);
    }

    #[test]
    fn memory_keeps_the_most_recent_fifty() {
        let mut memory = Memory::default();
        for ix in 0..60 {
            memory.put(key(&format!("C{ix}")), ix);
        }
        assert_eq!(memory.len(), REMEMBERED);
        assert!(memory.get(&key("C5")).is_none());
        assert_eq!(memory.get(&key("C59")), Some(&59));
        // Putting again moves it to the front.
        memory.put(key("C10"), 100);
        memory.put(key("C61"), 61);
        assert_eq!(memory.get(&key("C10")), Some(&100));
        assert_eq!(memory.take(&key("C10")), Some(100));
        assert!(memory.get(&key("C10")).is_none());
    }
}
