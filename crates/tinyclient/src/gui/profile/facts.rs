//! What a profile, a conversation's details and a Space's details show, decided from the data
//! alone, so it can be tested without a window. A section exists only with a value: nothing
//! here ever yields a label without one.

use crate::gui::time;
use protocol::{
    Activity, ActivityKind, CallSupport, Contact, ContactKind, Conversation, ConversationInfo, ConversationKind, Mark,
    Network, Person, Presence, Profile, Relation, Warning,
};

/// Role chips before "+n".
pub const ROLES_SHOWN: usize = 6;
/// Shared Spaces named before "n more".
pub const SPACES_SHOWN: usize = 5;
/// Rows in Right now.
pub const RIGHT_NOW: usize = 3;
/// A group DM lists its members inline up to this many; past it, one row opens the list.
pub const INLINE_MEMBERS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Card,
    Pane,
}

/// Where the person is seen from: what the shell knows that the profile does not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    /// The open conversation is the one with them.
    pub in_their_dm: bool,
    /// Their direct conversation, when one is held: how it calls and whether it is muted.
    pub direct: Option<(CallSupport, bool)>,
    /// Their network is connected. Off: what shows is the cache.
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Native,
    /// The official app carries it: the button's words.
    Handoff(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actions {
    pub message: bool,
    pub call: Option<Call>,
    /// A friend request waits: Accept and Ignore replace Message and Call.
    pub request: bool,
    /// The DM pane's Notifications switch, which carries mute there.
    pub notifications: bool,
    /// The "…" button. The card has none.
    pub more: bool,
}

pub fn handoff_words(network: Network) -> String {
    match network {
        Network::Slack => "Huddle in Slack".to_owned(),
        other => format!("Call in {}", other.label()),
    }
}

/// What the Call button says. Beside Message a hand-off is one word and its arrow (the full
/// words in its tooltip), so Message, Call and "…" fit the 268 row.
pub fn call_label(call: &Call, network: Network, beside_message: bool) -> String {
    match (call, beside_message) {
        (Call::Native, _) => "Call".to_owned(),
        (Call::Handoff(words), false) => words.clone(),
        (Call::Handoff(_), true) if network == Network::Slack => "Huddle".to_owned(),
        (Call::Handoff(_), true) => "Call".to_owned(),
    }
}

/// The buttons under a person's head, in the design's order.
///
/// One place per action: in their DM the conversation header carries Call (its pill is drawn
/// from the same `CallSupport`), so the pane and the card leave it out there.
pub fn actions(person: &Person, seen: &Seen, surface: Surface) -> Actions {
    let network = person.key.network;
    let request = person.relation == Relation::Incoming && network == Network::Discord;
    let gone = person.mark == Some(Mark::Deactivated);
    let call = match (request || person.bot || gone || seen.in_their_dm, seen.direct.map(|(call, _)| call)) {
        (true, _) => None,
        (false, Some(CallSupport::Native)) => Some(Call::Native),
        (false, Some(CallSupport::Handoff)) => Some(Call::Handoff(handoff_words(network))),
        (false, Some(CallSupport::None)) => None,
        // No direct conversation held yet: Discord rings from here; a Telegram person's call
        // opens in Telegram's own app, as their DM's does.
        (false, None) => match network {
            Network::Discord => Some(Call::Native),
            Network::Telegram if person.mark != Some(Mark::Verified) => Some(Call::Handoff(handoff_words(network))),
            _ => None,
        },
    };
    Actions {
        message: !request && !seen.in_their_dm && !gone,
        call,
        request,
        notifications: surface == Surface::Pane && seen.in_their_dm && seen.direct.is_some(),
        more: surface == Surface::Pane,
    }
}

/// The one heads-up line, by severity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadsUp {
    /// The network is unreachable: what shows is what this Mac holds. Outranks the rest.
    Offline,
    Flagged(Warning),
    Blocked,
    Away { until: Option<i64>, note: String },
    NotFriends,
}

pub fn heads_up(person: &Person, profile: Option<&Profile>, online: bool) -> Option<HeadsUp> {
    if !online {
        return Some(HeadsUp::Offline);
    }
    let marked = match person.mark {
        Some(Mark::Scam) => Some(Warning::Scam),
        Some(Mark::Fake) => Some(Warning::Fake),
        _ => None,
    };
    if let Some(warning) = profile.and_then(|profile| profile.warning).or(marked) {
        return Some(HeadsUp::Flagged(warning));
    }
    if person.relation == Relation::Blocked {
        return Some(HeadsUp::Blocked);
    }
    if let Some(profile) = profile.filter(|profile| !profile.away.is_empty() || profile.away_until.is_some()) {
        return Some(HeadsUp::Away { until: profile.away_until, note: profile.away.clone() });
    }
    // Discord is the network where a stranger's message can go unseen in requests.
    let stranger = person.key.network == Network::Discord && person.relation == Relation::None && !person.bot;
    stranger.then_some(HeadsUp::NotFriends)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Danger,
    Warning,
    Soft,
}

/// The heads-up's glyph, colour, words and second line.
pub struct HeadsUpWords {
    pub glyph: &'static str,
    pub tone: Tone,
    pub line: String,
    pub note: Option<String>,
}

// CONTRACT: the design's `shield-alert`, `ban` and `plane` glyphs are not in the shell's asset
// list (`gui/assets.rs`, slice A); the nearest ones stand in.
pub fn heads_up_words(heads_up: &HeadsUp, name: &str, network: Network, now: i64) -> HeadsUpWords {
    let net = network.label();
    match heads_up {
        HeadsUp::Offline => HeadsUpWords {
            glyph: "icons/wifi-off.svg",
            tone: Tone::Warning,
            line: crate::host::offline(net),
            note: None,
        },
        HeadsUp::Flagged(warning) => {
            let (line, note) = match warning {
                Warning::Scam => (format!("{net} flags this account as a scam."), "Never send codes or money."),
                Warning::Fake => (format!("{net} flags this account as fake."), "It may be pretending to be someone else."),
                Warning::Spam => (format!("{net} flags this account for spam."), "Never send codes or money."),
            };
            HeadsUpWords { glyph: "icons/triangle-alert.svg", tone: Tone::Danger, line, note: Some(note.to_owned()) }
        }
        HeadsUp::Blocked => {
            HeadsUpWords { glyph: "icons/circle-alert.svg", tone: Tone::Danger, line: format!("You blocked {name}"), note: None }
        }
        HeadsUp::Away { until, note } => HeadsUpWords {
            glyph: "icons/clock.svg",
            tone: Tone::Warning,
            line: match until {
                Some(until) if time::same_day(now, *until) => format!("Out of office until {}", time::clock(*until)),
                Some(until) => format!("Out of office until {}", short_day(*until)),
                None => "Out of office".to_owned(),
            },
            note: (!note.is_empty()).then(|| note.clone()),
        },
        HeadsUp::NotFriends => HeadsUpWords {
            glyph: "icons/info.svg",
            tone: Tone::Soft,
            line: "Not friends · they may not see your message".to_owned(),
            note: None,
        },
    }
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `Mon, Oct 19`: a day ahead, in the short form the heads-up uses.
pub fn short_day(unix: i64) -> String {
    let day = time::local(unix);
    format!("{}, {} {}", WEEKDAYS[day.weekday % 7], MONTHS[day.month % 12], day.day)
}

/// Today, tomorrow and yesterday in words; else the divider's words.
fn day_words(now: i64, unix: i64) -> String {
    if time::same_day(now + 86_400, unix) {
        return "tomorrow".to_owned();
    }
    match time::day(now, unix).as_str() {
        "Today" => "today".to_owned(),
        "Yesterday" => "yesterday".to_owned(),
        other => other.to_owned(),
    }
}

/// `Dec 2017`, from the formatter's `Dec 21, 2017`.
pub fn month_year(unix: i64) -> String {
    let date = time::date(unix);
    match (date.split_whitespace().next(), date.rsplit(' ').next()) {
        (Some(month), Some(year)) => format!("{month} {year}"),
        _ => date,
    }
}

/// `12,408`
pub fn thousands(count: u32) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The line under the name: handle and pronouns; Teams, which has no handle, its title.
pub fn handle_line(person: &Person, profile: Option<&Profile>) -> String {
    let handle = person.handle.trim_start_matches('@');
    let pronouns = profile.map_or("", |profile| profile.pronouns.as_str());
    if person.key.network == Network::Teams || handle.is_empty() {
        let title = profile.map(|profile| profile.title.as_str()).filter(|title| !title.is_empty()).unwrap_or(&person.title);
        if person.key.network == Network::Teams {
            return title.to_owned();
        }
        return pronouns.to_owned();
    }
    match pronouns {
        "" => format!("@{handle}"),
        pronouns => format!("@{handle} · {pronouns}"),
    }
}

/// The presence line: glyph shape, the word, what they say, until when.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresenceLine {
    /// The presence drawn as a glyph; `None` draws words only.
    pub glyph: Option<Presence>,
    pub word: String,
    pub status: String,
    pub until: String,
}

impl PresenceLine {
    pub fn is_empty(&self) -> bool {
        self.word.is_empty() && self.status.is_empty()
    }
}

/// `presence` and `status` are what is known now (the hub's, else the row's). A call takes the
/// place of the word while it lasts.
pub fn presence_line(
    person: &Person,
    profile: Option<&Profile>,
    presence: Presence,
    status: &str,
    in_call: bool,
    now: i64,
) -> PresenceLine {
    let network = person.key.network;
    let emoji = person.emoji.trim();
    let status = match (emoji.is_empty(), status.trim().is_empty()) {
        (true, _) => status.trim().to_owned(),
        (false, true) => emoji.to_owned(),
        (false, false) => format!("{emoji} {}", status.trim()),
    };
    let until = match profile.and_then(|profile| profile.status_until).filter(|_| !status.is_empty()) {
        Some(until) if time::same_day(now, until) => format!("until {}", time::clock(until)),
        Some(until) => format!("until {}", day_words(now, until)),
        None => String::new(),
    };
    if person.bot {
        // An app has no presence of its own: only a status it set.
        return PresenceLine { glyph: None, word: String::new(), status, until };
    }
    if in_call {
        return PresenceLine { glyph: Some(Presence::Online), word: "In a call".to_owned(), status, until };
    }
    let last_seen = profile.and_then(|profile| profile.last_seen).filter(|_| !presence.is_around());
    if let Some(seen) = last_seen {
        // One last seen: the exact time wins over Telegram's rough words, which then go.
        let status = if rough_last_seen(&status) { String::new() } else { status };
        let word = match day_words(now, seen).as_str() {
            "today" | "yesterday" => format!("Last seen {} at {}", day_words(now, seen), time::clock(seen)),
            day => format!("Last seen {day}"),
        };
        return PresenceLine { glyph: Some(Presence::Offline), word, status, until };
    }
    let word = crate::gui::people::presence::word(presence, network).to_owned();
    PresenceLine { glyph: (presence != Presence::Unknown).then_some(presence), word, status, until }
}

/// Telegram's own rough words for someone who hides their last seen ("last seen recently").
pub fn rough_last_seen(status: &str) -> bool {
    status.trim().to_lowercase().starts_with("last seen")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// Slack, Teams.
    Work { title: String, department: String },
    /// Discord, a Telegram group: who they are in the Space it was opened from.
    Place { nickname: String, place: String, roles: Vec<String>, more: usize },
}

/// `place` is the Space's name when the profile was asked from one; without it there is no
/// nickname or role to show.
pub fn role(person: &Person, profile: &Profile, place: Option<&str>) -> Option<Role> {
    let title = if profile.title.is_empty() { person.title.clone() } else { profile.title.clone() };
    if !title.is_empty() || !profile.department.is_empty() {
        return Some(Role::Work { title, department: profile.department.clone() });
    }
    let place = place.filter(|place| !place.is_empty())?;
    if profile.nickname.is_empty() && profile.roles.is_empty() {
        return None;
    }
    let shown = profile.roles.len().min(ROLES_SHOWN);
    Some(Role::Place {
        nickname: profile.nickname.clone(),
        place: place.to_owned(),
        roles: profile.roles[..shown].to_vec(),
        more: profile.roles.len() - shown,
    })
}

/// `+44 •••• ••42`: the country code and the last two digits; every other digit a dot.
pub fn mask(phone: &str) -> String {
    let code = if phone.starts_with('+') { phone.split_whitespace().next().unwrap_or("").len() } else { 0 };
    let digits = phone.char_indices().filter(|(ix, ch)| *ix >= code && ch.is_ascii_digit()).count();
    let mut seen = 0;
    phone
        .char_indices()
        .map(|(ix, ch)| {
            if !ch.is_ascii_digit() || ix < code {
                return ch;
            }
            seen += 1;
            if seen + 2 > digits {
                ch
            } else {
                '•'
            }
        })
        .collect()
}

/// The Contact section's lines. Never on Discord, which shares neither.
pub fn contacts(person: &Person, profile: &Profile) -> Vec<Contact> {
    if person.key.network == Network::Discord || person.bot {
        return Vec::new();
    }
    profile.contacts.iter().filter(|contact| !contact.value.trim().is_empty()).cloned().collect()
}

/// What a contact line reads: masked phones until Show.
pub fn contact_words(contact: &Contact, shown: bool) -> String {
    let masked = contact.kind == ContactKind::Phone || contact.masked;
    if masked && !shown {
        mask(&contact.value)
    } else {
        contact.value.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InCommon {
    pub names: Vec<String>,
    /// Beyond the names listed.
    pub more: u32,
    /// "4 groups in common": the network counted but named none.
    pub words: String,
}

pub fn place_word(network: Network, count: u32) -> &'static str {
    match (network, count) {
        (Network::Discord, 1) => "server",
        (Network::Discord, _) => "servers",
        (Network::Slack, 1) => "workspace",
        (Network::Slack, _) => "workspaces",
        (Network::Teams, 1) => "team",
        (Network::Teams, _) => "teams",
        (Network::Telegram, 1) => "group",
        (Network::Telegram, _) => "groups",
        (Network::Signal | Network::Messages | Network::Tiny, 1) => "group",
        (Network::Signal | Network::Messages | Network::Tiny, _) => "groups",
    }
}

pub fn in_common(network: Network, profile: &Profile) -> Option<InCommon> {
    let listed = profile.mutual_spaces.len() as u32;
    let total = profile.mutual_total.max(listed);
    if total == 0 {
        return None;
    }
    let shown = profile.mutual_spaces.len().min(SPACES_SHOWN);
    Some(InCommon {
        names: profile.mutual_spaces[..shown].to_vec(),
        more: total - shown as u32,
        words: format!("{total} {} in common", place_word(network, total)),
    })
}

/// One fact row: a glyph, words, softer words after, and what a click opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub glyph: &'static str,
    pub words: String,
    pub soft: String,
    pub opens: Option<protocol::Key>,
}

// CONTRACT: `gamepad-2`, `music` and `calendar` are not in the asset list either.
fn activity_fact(activity: &Activity) -> Fact {
    let name = activity.name.clone();
    let (glyph, words) = match activity.kind {
        ActivityKind::InVoice => ("icons/volume-2.svg", if name.is_empty() { "In voice".to_owned() } else { format!("In voice · {name}") }),
        ActivityKind::Playing => ("icons/monitor-up.svg", format!("Playing {name}")),
        ActivityKind::Listening => ("icons/headphones.svg", format!("Listening to {name}")),
        ActivityKind::Watching => ("icons/monitor-up.svg", format!("Watching {name}")),
        ActivityKind::Streaming => ("icons/monitor-up.svg", format!("Streaming {name}")),
        ActivityKind::Competing => ("icons/monitor-up.svg", format!("Competing in {name}")),
        ActivityKind::InCall => ("icons/phone.svg", "In a call".to_owned()),
        ActivityKind::InMeeting => ("icons/clock.svg", if name.is_empty() { "In a meeting".to_owned() } else { format!("In a meeting · {name}") }),
        ActivityKind::Presenting => ("icons/monitor-up.svg", "Presenting".to_owned()),
        ActivityKind::Huddle => ("icons/headphones.svg", if name.is_empty() { "In a huddle".to_owned() } else { format!("In a huddle in {name}") }),
    };
    let soft = if activity.place.is_empty() { String::new() } else { format!(" · {}", activity.place) };
    Fact { glyph, words: words.trim().to_owned(), soft, opens: activity.conversation.clone() }
}

/// Right now: what they are doing, and their clock. At most three rows; none for an app.
/// `said` is the presence line already drawn: what it says ("In a meeting") is not said twice.
pub fn right_now(person: &Person, profile: Option<&Profile>, said: &PresenceLine, now: i64, ours: i32) -> Vec<Fact> {
    if person.bot {
        return Vec::new();
    }
    let head = format!("{} {}", said.word, said.status).to_lowercase();
    let mut facts: Vec<Fact> = person
        .activity
        .iter()
        .map(|activity| activity_fact(activity))
        .filter(|fact| !head.contains(&fact.words.split(" · ").next().unwrap_or("").to_lowercase()))
        .collect();
    let offset = profile.and_then(|profile| profile.utc_offset).or(person.utc_offset);
    if let Some(offset) = offset.filter(|offset| *offset != ours) {
        let (clock, apart) = crate::gui::people::card::local_time_parts(now, offset, ours);
        facts.push(Fact { glyph: "icons/clock.svg", words: format!("{clock} local"), soft: format!(" · {apart}"), opens: None });
    }
    facts.truncate(RIGHT_NOW);
    facts
}

/// The faint line at the foot: since when, on the network, as friends, in the Space.
pub fn foot(person: &Person, profile: &Profile, place: Option<&str>) -> String {
    let network = person.key.network;
    let mut parts = Vec::new();
    if let Some(since) = profile.since {
        parts.push(format!("On {} since {}", network.label(), month_year(since)));
    }
    // "Friends since" only while they are a friend; an app is nobody's.
    if let Some(since) = profile.friends_since.filter(|_| person.relation == Relation::Friend && !person.bot) {
        parts.push(format!("Friends since {}", month_year(since)));
    }
    if let (Some(joined), Some(place)) = (profile.joined, place.filter(|place| !place.is_empty())) {
        parts.push(format!("In {place} since {}", month_year(joined)));
    }
    parts.join(" · ")
}

/// Which sections of the person pane have content, in the fixed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sections {
    pub right_now: bool,
    pub role: bool,
    pub about: bool,
    pub contact: bool,
    pub in_common: bool,
    pub foot: bool,
}

pub fn sections(person: &Person, profile: &Profile, place: Option<&str>, now: i64, ours: i32) -> Sections {
    Sections {
        right_now: !right_now(person, Some(profile), &PresenceLine::default(), now, ours).is_empty(),
        role: role(person, profile, place).is_some(),
        about: !profile.bio.trim().is_empty() || !profile.note.trim().is_empty(),
        contact: !contacts(person, profile).is_empty(),
        in_common: in_common(person.key.network, profile).is_some(),
        foot: !foot(person, profile, place).is_empty(),
    }
}

/// What the card shows below the actions: one of Role or About, Role first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardBody {
    Role(Role),
    About(String),
}

pub fn card_body(person: &Person, profile: &Profile, place: Option<&str>) -> Option<CardBody> {
    if let Some(role) = role(person, profile, place) {
        return Some(CardBody::Role(role));
    }
    let bio = profile.bio.trim();
    (!bio.is_empty()).then(|| CardBody::About(bio.to_owned()))
}

/// One item of a "…" menu, in the design's order; `Separator` between groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Mute { muted: bool },
    CopyHandle,
    CopyLink,
    CopyId,
    OpenIn,
    CloseDirect,
    Separator,
    RemoveFriend,
    Block,
    Leave,
}

/// Remove friend and Block, where the network has them.
// CONTRACT: nothing says which networks can block or unfriend; friends exist where a network
// reports one, and Block is offered where the network has a blocked list (Discord, Telegram).
pub fn offers(person: &Person) -> (bool, bool) {
    let remove = person.relation == Relation::Friend && person.key.network == Network::Discord;
    let block = matches!(person.key.network, Network::Discord | Network::Telegram)
        && person.relation != Relation::Blocked
        && !person.bot;
    (remove, block)
}

fn tidy(mut items: Vec<Item>) -> Vec<Item> {
    // No separator first, last, or twice.
    items.dedup();
    while items.first() == Some(&Item::Separator) {
        items.remove(0);
    }
    while items.last() == Some(&Item::Separator) {
        items.pop();
    }
    items
}

/// A person's "…" menu. `alt` shows Copy ID. `pane`: the menu of the profile pane, whose foot
/// already opens them in the network's app; a row's menu has no foot, so it says it here.
pub fn person_menu(person: &Person, seen: &Seen, alt: bool, pane: bool) -> Vec<Item> {
    let mut items = Vec::new();
    // Signal, Messages, and Tiny do not serve mute. A switch that snaps back is left out.
    if let Some((_, muted)) = seen.direct.filter(|_| !seen.in_their_dm && !matches!(person.key.network, Network::Signal | Network::Messages | Network::Tiny)) {
        items.extend([Item::Mute { muted }, Item::Separator]);
    }
    if !person.handle.trim_start_matches('@').is_empty() {
        items.push(Item::CopyHandle);
    }
    if !person.link.is_empty() {
        items.push(Item::CopyLink);
    }
    if alt {
        items.push(Item::CopyId);
    }
    // "Open in" is the pane's foot: one place, not the menu as well.
    if !pane && !person.link.is_empty() {
        items.push(Item::OpenIn);
    }
    if seen.in_their_dm && seen.direct.is_some() {
        items.push(Item::CloseDirect);
    }
    let (remove, block) = offers(person);
    items.push(Item::Separator);
    if remove {
        items.push(Item::RemoveFriend);
    }
    if block {
        items.push(Item::Block);
    }
    tidy(items)
}

/// A group DM, or a Telegram group or channel: the user can leave it from the pane.
pub fn can_leave(conversation: &Conversation) -> bool {
    // A linked Signal device cannot leave a group. The item stays off until it can.
    if conversation.key.network == Network::Signal {
        return false;
    }
    conversation.kind == ConversationKind::Group
        || (conversation.key.network == Network::Telegram
            && matches!(conversation.kind, ConversationKind::Channel | ConversationKind::Announcement))
}

pub fn conversation_menu(conversation: &Conversation, link: Option<&str>, alt: bool) -> Vec<Item> {
    let mut items = Vec::new();
    if link.is_some() {
        items.push(Item::CopyLink);
        // The foot opens it in the app where it draws one; a Telegram chat has no foot.
        if !foot_opens(conversation.key.network) {
            items.push(Item::OpenIn);
        }
    }
    if alt {
        items.push(Item::CopyId);
    }
    items.push(Item::Separator);
    if can_leave(conversation) {
        items.push(Item::Leave);
    }
    tidy(items)
}

/// Details' foot carries "Open in" for this network's conversations.
pub fn foot_opens(network: Network) -> bool {
    network != Network::Telegram
}

/// Where the network's own app opens this conversation, where that can be said without asking.
// CONTRACT: `Conversation` carries no link of its own; Discord's and Slack's are built from ids,
// Telegram's is `ConversationInfo.link`, Teams has none.
pub fn conversation_link(conversation: &Conversation, info: Option<&ConversationInfo>) -> Option<String> {
    let told = info.map(|info| info.link.clone()).filter(|link| !link.is_empty());
    let id = &conversation.key.id;
    match conversation.key.network {
        Network::Discord => {
            let place = conversation.space.as_deref().unwrap_or("@me");
            Some(format!("https://discord.com/channels/{place}/{id}"))
        }
        Network::Slack => conversation.space.as_ref().map(|team| format!("slack://channel?team={team}&id={id}")),
        Network::Telegram | Network::Teams => told,
        // Signal opens a chat only by a phone number or username, which a row does not carry.
        Network::Signal | Network::Messages | Network::Tiny => None,
    }
}

pub fn space_link(space: &protocol::Key) -> Option<String> {
    match space.network {
        Network::Discord => Some(format!("https://discord.com/channels/{}", space.id)),
        Network::Slack => Some(format!("slack://open?team={}", space.id)),
        Network::Telegram | Network::Teams | Network::Signal | Network::Messages | Network::Tiny => None,
    }
}

/// What the one confirm asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Block,
    RemoveFriend,
    Leave,
}

pub fn confirm_words(act: Act, name: &str, network: Network) -> (String, String, &'static str) {
    let net = network.label();
    match act {
        Act::Block => (
            format!("Block {name} on {net}?"),
            match network {
                Network::Discord => "They won't be able to message you, and their messages in shared servers are hidden. You can unblock them in Discord.".to_owned(),
                _ => format!("They won't be able to message or call you on {net}. You can unblock them in {net}."),
            },
            "Block",
        ),
        Act::RemoveFriend => (
            format!("Remove {name} as a friend on {net}?"),
            "You'll stay in the servers you share. They are not told.".to_owned(),
            "Remove friend",
        ),
        Act::Leave => (
            format!("Leave {name} on {net}?"),
            "You'll stop getting its messages. To come back, someone in it has to add you.".to_owned(),
            "Leave",
        ),
    }
}

/// How the Notifications row is drawn for a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyControl {
    /// On or off: DMs, group DMs, Telegram.
    Switch,
    /// All · Mentions · Nothing: channels on Discord and Slack. Teams is All · Nothing.
    Segmented,
    /// The phone or the server owns this. The words are shown and nothing is sent.
    Held(&'static str),
}

pub fn notify_control(conversation: &Conversation) -> NotifyControl {
    match conversation.key.network {
        Network::Signal => NotifyControl::Held("Set on your phone"),
        Network::Messages => NotifyControl::Held("Stays on your phone"),
        Network::Tiny => NotifyControl::Held("Not available on Tiny"),
        _ => {
            let channel = matches!(
                conversation.kind,
                ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Voice | ConversationKind::Forum
            );
            if channel && conversation.key.network != Network::Telegram {
                NotifyControl::Segmented
            } else {
                NotifyControl::Switch
            }
        }
    }
}

/// The line under a conversation's name in Details.
pub fn conversation_line(conversation: &Conversation, info: Option<&ConversationInfo>) -> String {
    let members = info.and_then(|info| info.members);
    let online = info.and_then(|info| info.online).filter(|online| *online > 0);
    match (conversation.key.network, conversation.kind) {
        (_, ConversationKind::Group) => match members {
            Some(count) => format!("Group · {count} people"),
            None => "Group".to_owned(),
        },
        (Network::Telegram, kind) => {
            let link = info.map(|info| info.link.as_str()).unwrap_or("");
            let handle = link.rsplit('/').next().filter(|_| link.contains("t.me/")).map(|name| format!("@{name}"));
            let (noun, people) = match kind {
                ConversationKind::Announcement => ("Channel", "subscribers"),
                _ => ("Group", "members"),
            };
            let mut parts: Vec<String> = handle.into_iter().collect();
            parts.push(noun.to_owned());
            if let Some(count) = members {
                parts.push(format!("{} {people}", thousands(count)));
            }
            if let (Some(online), ConversationKind::Channel) = (online, kind) {
                parts.push(format!("{} online", thousands(online)));
            }
            parts.join(" · ")
        }
        _ => {
            let path: Vec<&str> =
                [conversation.space_title.as_str(), conversation.group_title.as_str()].into_iter().filter(|part| !part.is_empty()).collect();
            path.join(" › ")
        }
    }
}

/// The members part of Details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembersShown {
    /// A small group: its people as rows.
    Inline,
    /// "Members 128 · 12 online", which opens the member list.
    Row { members: u32, online: Option<u32> },
    /// A Telegram channel: its count stays in the head.
    None,
}

pub fn members_shown(conversation: &Conversation, info: Option<&ConversationInfo>) -> MembersShown {
    let members = info.and_then(|info| info.members);
    match (conversation.kind, members) {
        (_, None) => MembersShown::None,
        (ConversationKind::Announcement, _) if conversation.key.network == Network::Telegram => MembersShown::None,
        (ConversationKind::Group, Some(count)) if count <= INLINE_MEMBERS => MembersShown::Inline,
        (_, Some(members)) => MembersShown::Row { members, online: info.and_then(|info| info.online).filter(|online| *online > 0) },
    }
}

/// A member list's own count, when it is the whole list: fewer came back than were asked.
pub fn listed_count(listed: usize, asked: u32) -> Option<u32> {
    (listed > 0 && (listed as u32) < asked).then_some(listed as u32)
}

/// "Created by Dev Patel · Mar 2024", "Created Mar 2024".
pub fn created_line(info: &ConversationInfo) -> String {
    match (info.created, info.creator.trim()) {
        (Some(created), "") => format!("Created {}", month_year(created)),
        (Some(created), creator) => format!("Created by {creator} · {}", month_year(created)),
        (None, "") => String::new(),
        (None, creator) => format!("Created by {creator}"),
    }
}

/// The tag after a conversation's name.
pub fn conversation_tag(conversation: &Conversation, info: Option<&ConversationInfo>) -> Option<String> {
    if let Some(external) = info.map(|info| info.external.trim()).filter(|external| !external.is_empty()) {
        return Some(match conversation.key.network {
            Network::Slack => format!("External · {external}"),
            _ => "Shared".to_owned(),
        });
    }
    // Signal marks every chat private because the chat is encrypted. That is not a private channel.
    if conversation.private && conversation.key.network != Network::Signal {
        return Some("Private".to_owned());
    }
    conversation.read_only.then(|| "Read-only".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Key;

    fn person(network: Network, relation: Relation) -> Person {
        let mut person = Person::new(Key { network, account: "1".into(), id: "p".into() }, "Juno Castellane");
        person.relation = relation;
        person.handle = "juno.c".into();
        person.link = "https://example.org/u/p".into();
        person
    }

    fn row(network: Network, kind: ConversationKind) -> Conversation {
        Conversation {
            key: Key { network, account: "1".into(), id: "c1".into() },
            kind,
            title: "Mushroom run".into(),
            space: None,
            space_title: String::new(),
            group: None,
            group_title: String::new(),
            topic: String::new(),
            parent: None,
            root: None,
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
            occupants: None,
            limit: 0,
            read_only: false,
            private: false,
            draft: String::new(),
            folders: Vec::new(),
            archived: false,
            pin: None,
            hidden: false,
            followed: false,
            bot: false,
            meeting: false,
            special: Default::default(),
            mark: None,
            topic_icon: None,
            opted_out: false,
            quiet: false,
            preview_from: String::new(),
            preview_mine: false,
            seen: false,
            delivered: false,
            sending: false,
        }
    }

    const DM: Seen = Seen { in_their_dm: true, direct: Some((CallSupport::Native, false)), online: true };

    #[test]
    fn inside_their_dm_there_is_no_message_button_and_mute_is_the_switch() {
        let juno = person(Network::Discord, Relation::Friend);
        let pane = actions(&juno, &DM, Surface::Pane);
        assert!(!pane.message);
        // The DM's header carries Call: one place per action.
        assert_eq!(pane.call, None);
        assert!(pane.notifications && pane.more);
        assert_eq!(actions(&juno, &Seen { in_their_dm: false, ..DM }, Surface::Pane).call, Some(Call::Native));
        // From a channel, the same person offers Message, and the card never has "…".
        let elsewhere = Seen { in_their_dm: false, ..DM };
        assert!(actions(&juno, &elsewhere, Surface::Pane).message);
        let card = actions(&juno, &elsewhere, Surface::Card);
        assert!(card.message && !card.more && !card.notifications);
    }

    #[test]
    fn calls_are_native_handed_off_or_absent_by_network_and_never_for_apps() {
        let seen = |call| Seen { in_their_dm: false, direct: Some((call, false)), online: true };
        let tess = person(Network::Slack, Relation::Contact);
        assert_eq!(actions(&tess, &seen(CallSupport::Handoff), Surface::Pane).call, Some(Call::Handoff("Huddle in Slack".into())));
        let omar = person(Network::Teams, Relation::Contact);
        assert_eq!(actions(&omar, &seen(CallSupport::Handoff), Surface::Pane).call, Some(Call::Handoff("Call in Teams".into())));
        assert_eq!(actions(&omar, &seen(CallSupport::None), Surface::Pane).call, None);
        // No DM held: only Discord rings by itself.
        let unseen = Seen { online: true, ..Seen::default() };
        assert_eq!(actions(&tess, &unseen, Surface::Pane).call, None);
        assert_eq!(actions(&person(Network::Discord, Relation::None), &unseen, Surface::Pane).call, Some(Call::Native));
        // A Telegram person's call is a hand-off, DM held or not; their service account has none.
        let nadia = person(Network::Telegram, Relation::Contact);
        assert_eq!(actions(&nadia, &unseen, Surface::Card).call, Some(Call::Handoff("Call in Telegram".into())));
        assert_eq!(actions(&nadia, &seen(CallSupport::Handoff), Surface::Pane).call, Some(Call::Handoff("Call in Telegram".into())));
        let mut service = nadia.clone();
        service.mark = Some(Mark::Verified);
        assert_eq!(actions(&service, &unseen, Surface::Card).call, None);
        let mut bot = person(Network::Discord, Relation::None);
        bot.bot = true;
        assert_eq!(actions(&bot, &seen(CallSupport::Native), Surface::Pane).call, None);
        // A request waiting replaces both.
        let asking = actions(&person(Network::Discord, Relation::Incoming), &unseen, Surface::Pane);
        assert!(asking.request && !asking.message && asking.call.is_none());
    }

    #[test]
    fn one_heads_up_by_severity() {
        let mut fake = person(Network::Telegram, Relation::Contact);
        fake.mark = Some(Mark::Fake);
        let mut away = Profile::of(fake.clone());
        away.away = "At a conference".into();
        assert_eq!(heads_up(&fake, Some(&away), true), Some(HeadsUp::Flagged(Warning::Fake)));
        assert_eq!(heads_up(&fake, Some(&away), false), Some(HeadsUp::Offline));
        let omar = person(Network::Teams, Relation::Contact);
        assert_eq!(heads_up(&omar, Some(&away), true), Some(HeadsUp::Away { until: None, note: "At a conference".into() }));
        assert_eq!(heads_up(&omar, None, true), None);
        assert_eq!(heads_up(&person(Network::Discord, Relation::Blocked), None, true), Some(HeadsUp::Blocked));
        assert_eq!(heads_up(&person(Network::Discord, Relation::None), None, true), Some(HeadsUp::NotFriends));
        assert_eq!(heads_up(&person(Network::Discord, Relation::Friend), None, true), None);
        let words = heads_up_words(&HeadsUp::Flagged(Warning::Scam), "x", Network::Telegram, 0);
        assert_eq!((words.line.as_str(), words.tone), ("Telegram flags this account as a scam.", Tone::Danger));
    }

    #[test]
    fn phones_are_masked_but_for_the_country_code_and_last_two_digits() {
        assert_eq!(mask("+44 7700 900142"), "+44 •••• ••••42");
        assert_eq!(mask("+385 91 234 5617"), "+385 •• ••• ••17");
        assert_eq!(mask("555-0142"), "•••-••42");
        let phone = Contact { kind: ContactKind::Phone, value: "+351 912 345 017".into(), masked: true };
        assert_eq!(contact_words(&phone, false), "+351 ••• ••• •17");
        assert_eq!(contact_words(&phone, true), "+351 912 345 017");
        let email = Contact { kind: ContactKind::Email, value: "tess@harbor.example".into(), masked: false };
        assert_eq!(contact_words(&email, false), "tess@harbor.example");
    }

    #[test]
    fn a_section_shows_only_with_content_and_contact_never_on_discord() {
        let juno = person(Network::Discord, Relation::Friend);
        let mut profile = Profile::of(juno.clone());
        assert_eq!(sections(&juno, &profile, None, 0, 0), Sections::default());
        profile.contacts = vec![Contact { kind: ContactKind::Email, value: "a@b".into(), masked: false }];
        profile.nickname = "Ju".into();
        profile.roles = (0..8).map(|n| format!("r{n}")).collect();
        let shown = sections(&juno, &profile, None, 0, 0);
        // Roles need the Space the profile was asked from; contacts are never drawn on Discord.
        assert!(!shown.role && !shown.contact);
        let Some(Role::Place { roles, more, .. }) = role(&juno, &profile, Some("Moss & Lichen")) else { panic!("a place role") };
        assert_eq!((roles.len(), more), (6, 2));
        let tess = person(Network::Slack, Relation::Contact);
        let mut slack = Profile::of(tess.clone());
        slack.title = "Staff Engineer, Payments".into();
        slack.contacts = profile.contacts.clone();
        let shown = sections(&tess, &slack, None, 0, 0);
        assert!(shown.role && shown.contact && !shown.about);
        assert_eq!(card_body(&tess, &slack, None), Some(CardBody::Role(Role::Work { title: "Staff Engineer, Payments".into(), department: String::new() })));
    }

    #[test]
    fn the_line_under_the_name_is_the_handle_or_on_teams_the_title() {
        let juno = person(Network::Discord, Relation::Friend);
        let mut profile = Profile::of(juno.clone());
        assert_eq!(handle_line(&juno, Some(&profile)), "@juno.c");
        profile.pronouns = "they/them".into();
        assert_eq!(handle_line(&juno, Some(&profile)), "@juno.c · they/them");
        let mut omar = person(Network::Teams, Relation::Contact);
        omar.handle = "omar@contoso.com".into();
        omar.title = "Product Designer".into();
        assert_eq!(handle_line(&omar, None), "Product Designer");
    }

    #[test]
    fn in_common_names_five_and_counts_what_it_cannot_name() {
        let juno = person(Network::Discord, Relation::Friend);
        let mut profile = Profile::of(juno);
        assert_eq!(in_common(Network::Discord, &profile), None);
        profile.mutual_spaces = (0..7).map(|n| format!("S{n}")).collect();
        let common = in_common(Network::Discord, &profile).expect("some");
        assert_eq!((common.names.len(), common.more, common.words.as_str()), (5, 2, "7 servers in common"));
        profile.mutual_spaces.clear();
        profile.mutual_total = 4;
        let common = in_common(Network::Telegram, &profile).expect("counted");
        assert_eq!((common.names.len(), common.words.as_str()), (0, "4 groups in common"));
    }

    #[test]
    fn menus_follow_the_designs_order_and_destructive_items_come_last() {
        let juno = person(Network::Discord, Relation::Friend);
        let from_channel = Seen { in_their_dm: false, direct: Some((CallSupport::Native, true)), online: true };
        assert_eq!(
            person_menu(&juno, &from_channel, true, false),
            vec![Item::Mute { muted: true }, Item::Separator, Item::CopyHandle, Item::CopyLink, Item::CopyId, Item::OpenIn, Item::Separator, Item::RemoveFriend, Item::Block]
        );
        // In their DM: no Mute (the switch has it), Close DM instead, Copy ID only with ⌥. The
        // pane's foot opens them in the app, so its menu does not.
        assert_eq!(person_menu(&juno, &DM, false, true), vec![Item::CopyHandle, Item::CopyLink, Item::CloseDirect, Item::Separator, Item::RemoveFriend, Item::Block]);
        let tess = person(Network::Slack, Relation::Contact);
        assert_eq!(person_menu(&tess, &Seen::default(), false, false), vec![Item::CopyHandle, Item::CopyLink, Item::OpenIn]);
        assert_eq!(person_menu(&tess, &Seen::default(), false, true), vec![Item::CopyHandle, Item::CopyLink]);
        let group = row(Network::Discord, ConversationKind::Group);
        assert_eq!(conversation_menu(&group, Some("l"), false), vec![Item::CopyLink, Item::Separator, Item::Leave]);
        // A Telegram chat has no foot, so its menu opens it.
        assert_eq!(conversation_menu(&row(Network::Telegram, ConversationKind::Group), Some("l"), false), vec![Item::CopyLink, Item::OpenIn, Item::Separator, Item::Leave]);
        let channel = row(Network::Slack, ConversationKind::Channel);
        assert_eq!(conversation_menu(&channel, None, false), Vec::<Item>::new());
        assert!(can_leave(&row(Network::Telegram, ConversationKind::Announcement)));
        assert!(!can_leave(&row(Network::Discord, ConversationKind::Channel)));
    }

    #[test]
    fn details_read_by_kind_and_network() {
        let mut info = ConversationInfo {
            conversation: row(Network::Telegram, ConversationKind::Announcement),
            description: String::new(),
            members: Some(12_408),
            online: None,
            created: None,
            creator: String::new(),
            notify: Default::default(),
            pinned: None,
            external: String::new(),
            link: "https://t.me/riverbend_news".into(),
        };
        let channel = info.conversation.clone();
        assert_eq!(conversation_line(&channel, Some(&info)), "@riverbend_news · Channel · 12,408 subscribers");
        assert_eq!(members_shown(&channel, Some(&info)), MembersShown::None);
        assert_eq!(notify_control(&channel), NotifyControl::Switch);
        let group = row(Network::Discord, ConversationKind::Group);
        info.members = Some(4);
        assert_eq!(conversation_line(&group, Some(&info)), "Group · 4 people");
        assert_eq!(members_shown(&group, Some(&info)), MembersShown::Inline);
        let mut slack = row(Network::Slack, ConversationKind::Channel);
        slack.space_title = "Harbor Labs".into();
        slack.private = true;
        info.members = Some(128);
        info.online = Some(12);
        assert_eq!(members_shown(&slack, Some(&info)), MembersShown::Row { members: 128, online: Some(12) });
        assert_eq!(notify_control(&slack), NotifyControl::Segmented);
        assert_eq!(conversation_line(&slack, None), "Harbor Labs");
        assert_eq!(conversation_tag(&slack, None).as_deref(), Some("Private"));
        let mut signal = row(Network::Signal, ConversationKind::Direct);
        signal.private = true;
        assert_eq!(conversation_tag(&signal, None), None);
        assert_eq!(notify_control(&signal), NotifyControl::Held("Set on your phone"));
        assert!(!can_leave(&row(Network::Signal, ConversationKind::Group)));
        assert_eq!(notify_control(&row(Network::Messages, ConversationKind::Direct)), NotifyControl::Held("Stays on your phone"));
        assert_eq!(notify_control(&row(Network::Teams, ConversationKind::Channel)), NotifyControl::Segmented);
        info.external = "Northwind".into();
        assert_eq!(conversation_tag(&slack, Some(&info)).as_deref(), Some("External · Northwind"));
        info.creator = "Dev Patel".into();
        assert!(created_line(&info).is_empty() || created_line(&info) == "Created by Dev Patel");
        assert_eq!(thousands(1_204), "1,204");
        assert_eq!(thousands(999), "999");
    }

    #[test]
    fn telegram_says_last_seen_once_the_exact_time_over_its_rough_words() {
        let mut haruto = person(Network::Telegram, Relation::Contact);
        haruto.status = "last seen recently".into();
        let now = 1_760_356_800;
        let rough = presence_line(&haruto, None, Presence::Unknown, "last seen recently", false, now);
        assert_eq!((rough.word.as_str(), rough.status.as_str(), rough.glyph), ("", "last seen recently", None));
        let mut profile = Profile::of(haruto.clone());
        profile.last_seen = Some(now - 600);
        let exact = presence_line(&haruto, Some(&profile), Presence::Unknown, "last seen recently", false, now);
        assert!(exact.word.starts_with("Last seen today at "), "{}", exact.word);
        assert_eq!(exact.status, "");
        // A status of their own stays beside it.
        let own = presence_line(&haruto, Some(&profile), Presence::Unknown, "in Lisbon", false, now);
        assert_eq!(own.status, "in Lisbon");
    }

    fn act(kind: ActivityKind) -> Activity {
        Activity { kind, name: String::new(), place: String::new(), conversation: None }
    }

    #[test]
    fn right_now_does_not_repeat_the_presence_line() {
        let mut omar = person(Network::Teams, Relation::Contact);
        omar.activity = Some(Box::new(act(ActivityKind::InMeeting)));
        let said = PresenceLine { glyph: Some(Presence::Busy), word: "Busy".into(), status: "In a meeting".into(), until: String::new() };
        assert!(right_now(&omar, None, &said, 0, 0).is_empty());
        let plain = PresenceLine { glyph: Some(Presence::Busy), word: "Busy".into(), ..PresenceLine::default() };
        assert_eq!(right_now(&omar, None, &plain, 0, 0).len(), 1);
        omar.activity = Some(Box::new(act(ActivityKind::Presenting)));
        let presenting = PresenceLine { word: "Presenting".into(), ..PresenceLine::default() };
        assert!(right_now(&omar, None, &presenting, 0, 0).is_empty());
    }

    #[test]
    fn friends_since_only_while_a_friend_and_never_for_an_app() {
        let mut profile = Profile::of(person(Network::Discord, Relation::Friend));
        profile.friends_since = Some(1_620_000_000);
        assert!(foot(&person(Network::Discord, Relation::Friend), &profile, None).starts_with("Friends since"));
        assert_eq!(foot(&person(Network::Discord, Relation::None), &profile, None), "");
        let mut bot = person(Network::Discord, Relation::Friend);
        bot.bot = true;
        assert_eq!(foot(&bot, &profile, None), "");
    }

    #[test]
    fn out_of_office_reads_a_short_day_and_a_full_list_counts_itself() {
        let now = 1_760_356_800;
        let words = heads_up_words(&HeadsUp::Away { until: Some(now + 6 * 86_400), note: String::new() }, "x", Network::Teams, now);
        let day = words.line.trim_start_matches("Out of office until ");
        // "Mon, Oct 19": weekday, comma, month and day.
        let parts: Vec<&str> = day.split(' ').collect();
        assert_eq!(parts.len(), 3, "{day}");
        assert!(parts[0].ends_with(',') && parts[0].len() == 4 && parts[1].len() == 3, "{day}");
        assert_eq!(listed_count(4, INLINE_MEMBERS), Some(4));
        assert_eq!(listed_count(10, INLINE_MEMBERS), None);
        assert_eq!(listed_count(0, INLINE_MEMBERS), None);
    }

    #[test]
    fn a_hand_off_beside_message_is_one_word() {
        let handoff = Call::Handoff(handoff_words(Network::Telegram));
        assert_eq!(call_label(&handoff, Network::Telegram, false), "Call in Telegram");
        assert_eq!(call_label(&handoff, Network::Telegram, true), "Call");
        assert_eq!(call_label(&Call::Handoff(handoff_words(Network::Slack)), Network::Slack, true), "Huddle");
        assert_eq!(call_label(&Call::Native, Network::Discord, false), "Call");
    }
}
