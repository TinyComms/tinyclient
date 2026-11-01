//! `--shot … --demo`: a made-up chatd, so every screen can be drawn full of believable data
//! without starting chatd or reading anyone's real chats. Compiled with `gui-shot`, and with
//! `demo` for phone proof builds, where it is on from launch (see `on`).
//!
//! It answers at the one door every request goes through (`client::ask_within`), so the window
//! takes the same paths it takes with chatd: `Status`, the Inbox, the trees, pages of messages,
//! people, profiles. Nothing is stored and nothing is sent; a request that would change something
//! is answered `Ok` and forgotten.
//!
//! Everyone and everything here is invented. Portraits, icons and photographs come from the
//! chatd demo's own public-domain set (`crates/chatd/src/demo/photos`, credited there).
//!
//! `--demo-empty` is the same made-up chatd with nothing signed in: the first-run screen.

use super::time::until_midnight;
use crate::view::now;
use protocol::{
    AccountKey, AccountStatus, Attachment, Capabilities, Conversation, ConversationInfo, ConversationKind,
    Group, GroupKind, InboxCounts, InboxFilter, Key, Link, Media, MediaFile, MediaKind, MemberSection, Message,
    MessageHit, Network, NetworkStatus, Notify, Occupant, Occupants, Page, Person, Presence, Preview, Profile, Reaction,
    Relation, ReplyTo, Request, Response, Sender, Space, SpaceInfo, Span, Style, MentionKind,
};
use std::sync::OnceLock;
use std::time::Duration;

/// `--demo` was passed, or this is a `demo` build on a phone (which has no flags): the made-up
/// chatd answers instead of the real one.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| on_phone() || std::env::args().any(|arg| arg == "--demo" || arg == "--demo-empty"))
}

/// A `demo` build on Android or iOS: on from launch.
fn on_phone() -> bool {
    cfg!(all(feature = "demo", any(target_os = "android", target_os = "ios")))
}

fn empty() -> bool {
    static EMPTY: OnceLock<bool> = OnceLock::new();
    *EMPTY.get_or_init(|| std::env::args().any(|arg| arg == "--demo-empty"))
}

/// Where the chatd demo keeps its pictures. Read from the source tree, as the demo itself does.
#[cfg(not(all(feature = "demo", any(target_os = "android", target_os = "ios"))))]
fn photo(name: &str) -> String {
    format!("{}/../chatd/src/demo/photos/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// On a phone the source tree is not there, so the pictures are compiled in and written once to
/// the app's support directory, where the window reads them like any downloaded file.
#[cfg(all(feature = "demo", any(target_os = "android", target_os = "ios")))]
fn photo(name: &str) -> String {
    macro_rules! photos {
        ($($file:literal),* $(,)?) => {
            &[$(($file, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../chatd/src/demo/photos/", $file)) as &[u8])),*]
        };
    }
    static PHOTOS: &[(&str, &[u8])] = photos!(
        "avatar-ana-ruiz.jpg", "avatar-dita-novak.jpg", "avatar-grandma-rosa.jpg", "avatar-halvard-eide.jpg",
        "avatar-ines-marchetti.jpg", "avatar-kofi-asante.jpg", "avatar-mara-okafor.jpg", "avatar-priya-raman.jpg",
        "avatar-robin-vale.jpg", "avatar-tomasz-brandt.jpg", "avatar-yusuf-demirci.jpg", "group-design-crit.webp",
        "group-foray-carpool.webp", "icon-moss-lichen.webp", "icon-night-shift.webp", "logo-harbor-labs.png",
        "moss-frost.webp", "preview-creek.webp", "weir-dawn.webp",
    );
    let dir = protocol::support_dir().join("demo-photos");
    let path = dir.join(name);
    if !path.exists() {
        if let Some((_, bytes)) = PHOTOS.iter().find(|(file, _)| *file == name) {
            let _ = std::fs::create_dir_all(&dir);
            if let Err(err) = std::fs::write(&path, bytes) {
                log::warn!("tinyclient demo: could not write {}: {err}", path.display());
            }
        } else {
            log::warn!("tinyclient demo: no compiled-in picture {name}");
        }
    }
    path.to_string_lossy().into_owned()
}

fn picture(id: &str, kind: MediaKind, name: &str, width: u32, height: u32) -> MediaFile {
    MediaFile { width, height, bytes: 48_000, path: Some(photo(name)), ..MediaFile::new(id, kind) }
}

fn face(id: &str, name: &str) -> MediaFile {
    picture(&format!("avatar-{id}"), MediaKind::Avatar, name, 192, 192)
}

// Time. Today's times are minutes ago, never before this morning; older ones are a time of day.

fn midnight() -> i64 {
    let at = now();
    at + until_midnight(at) as i64 - 86_400
}

/// `minutes` ago, kept inside today so a shot taken just after midnight still reads "today".
fn ago(minutes: i64) -> i64 {
    (now() - minutes * 60).max(midnight() + 60 + (1_000 - minutes.min(999)))
}

/// `days` back, at `hour:minute` local time.
fn on_day(days: i64, hour: i64, minute: i64) -> i64 {
    midnight() - days * 86_400 + hour * 3600 + minute * 60
}

// Accounts.

const ME: &str = "robin";
const ME_NAME: &str = "Robin Vale";

fn tiny() -> AccountKey {
    AccountKey { network: Network::Tiny, account: "premier.tiny.example".into() }
}

fn discord() -> AccountKey {
    AccountKey { network: Network::Discord, account: "d-robin".into() }
}

fn slack() -> AccountKey {
    AccountKey { network: Network::Slack, account: "T0HARBOR".into() }
}

fn account(key: AccountKey, name: &str, attention: u32, unread: bool, presence: Presence) -> AccountStatus {
    AccountStatus {
        key,
        name: name.into(),
        link: Link::Connected,
        detail: String::new(),
        me: ME.into(),
        attention,
        unread,
        reconnecting: false,
        required: String::new(),
        avatar: Some(face("robin-vale", "avatar-robin-vale.jpg")),
        presence,
        status: String::new(),
        can: Capabilities { max_file_bytes: 25 * 1024 * 1024, max_files: 10, ..Capabilities::default() },
    }
}

// People.

struct Who {
    id: &'static str,
    name: &'static str,
    network: Network,
    presence: Presence,
    photo: Option<&'static str>,
    status: &'static str,
    title: &'static str,
}

const PEOPLE: &[Who] = &[
    Who { id: "yusuf", name: "Yusuf Demirci", network: Network::Discord, presence: Presence::Online, photo: Some("avatar-yusuf-demirci.jpg"), status: "Out with the trail cams", title: "" },
    Who { id: "ines", name: "Ines Marchetti", network: Network::Discord, presence: Presence::Online, photo: Some("avatar-ines-marchetti.jpg"), status: "", title: "" },
    Who { id: "tomasz", name: "Tomasz Brandt", network: Network::Discord, presence: Presence::Idle, photo: Some("avatar-tomasz-brandt.jpg"), status: "", title: "" },
    Who { id: "kofi", name: "Kofi Asante", network: Network::Discord, presence: Presence::Offline, photo: Some("avatar-kofi-asante.jpg"), status: "", title: "" },
    Who { id: "dita", name: "Dita Novak", network: Network::Discord, presence: Presence::Dnd, photo: Some("avatar-dita-novak.jpg"), status: "Keying lichens", title: "" },
    Who { id: "mara", name: "Mara Okafor", network: Network::Slack, presence: Presence::Online, photo: Some("avatar-mara-okafor.jpg"), status: "", title: "Staff Engineer, Platform" },
    Who { id: "priya", name: "Priya Raman", network: Network::Slack, presence: Presence::Idle, photo: Some("avatar-priya-raman.jpg"), status: "", title: "Product Designer" },
    Who { id: "imogen", name: "Imogen Castellanos-Whitfield", network: Network::Slack, presence: Presence::Offline, photo: None, status: "", title: "Head of Product" },
    Who { id: "theo", name: "Theo Marchetti", network: Network::Slack, presence: Presence::Online, photo: None, status: "", title: "Design Lead" },
    Who { id: "halvard", name: "Halvard Eide", network: Network::Tiny, presence: Presence::Online, photo: Some("avatar-halvard-eide.jpg"), status: "", title: "" },
    Who { id: "ana", name: "Ana Ruiz", network: Network::Tiny, presence: Presence::Dnd, photo: Some("avatar-ana-ruiz.jpg"), status: "Focus time", title: "" },
    Who { id: "rosa", name: "Grandma Rosa", network: Network::Tiny, presence: Presence::Offline, photo: Some("avatar-grandma-rosa.jpg"), status: "", title: "" },
];

fn account_of(network: Network) -> AccountKey {
    match network {
        Network::Discord => discord(),
        Network::Slack => slack(),
        _ => tiny(),
    }
}

fn who(id: &str) -> Option<&'static Who> {
    PEOPLE.iter().find(|who| who.id == id)
}

fn person_of(who: &Who) -> Person {
    let mut person = Person::new(Key::new(&account_of(who.network), who.id), who.name);
    person.handle = who.name.to_lowercase().replace(' ', ".");
    person.avatar = who.photo.map(|name| face(who.id, name));
    person.presence = who.presence;
    person.status = who.status.into();
    person.title = who.title.into();
    person.relation = if who.network == Network::Discord { Relation::Friend } else { Relation::Contact };
    person
}

fn me_on(account: &AccountKey) -> Person {
    let mut person = Person::new(Key::new(account, ME), ME_NAME);
    person.handle = "robin.vale".into();
    person.avatar = Some(face("robin-vale", "avatar-robin-vale.jpg"));
    person.presence = Presence::Online;
    person
}

fn sender(id: &str) -> Sender {
    if id == ME {
        return Sender { id: ME.into(), name: ME_NAME.into(), avatar: Some(face("robin-vale", "avatar-robin-vale.jpg")), bot: false };
    }
    let who = who(id).expect("a person of the fixture");
    Sender { id: id.into(), name: who.name.into(), avatar: who.photo.map(|name| face(who.id, name)), bot: false }
}

// Conversations.

struct Fixture {
    accounts: Vec<AccountStatus>,
    spaces: Vec<Space>,
    groups: Vec<(Key, Vec<Group>)>,
    /// Every row, the Inbox's and the trees' alike.
    rows: Vec<Conversation>,
    /// Which rows the Inbox lists.
    inbox: Vec<Key>,
    messages: Vec<(Key, Vec<Message>)>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(build)
}

fn row(account: &AccountKey, id: &str, kind: ConversationKind, title: &str, updated: i64) -> Conversation {
    let mut row = Conversation::new(Key::new(account, id), kind, title);
    row.updated = updated;
    row
}

fn in_space(mut row: Conversation, space: &str, space_title: &str, group: &str, group_title: &str, position: i32) -> Conversation {
    row.space = Some(space.into());
    row.space_title = space_title.into();
    row.group = Some(group.into());
    row.group_title = group_title.into();
    row.position = position;
    row
}

fn direct(account: &AccountKey, id: &str, peer: &str, updated: i64) -> Conversation {
    let who = who(peer).expect("a person of the fixture");
    let mut row = row(account, id, ConversationKind::Direct, who.name, updated);
    row.peer = Some(peer.into());
    row.avatar = who.photo.map(|name| face(who.id, name));
    row.call = if account.network == Network::Tiny { protocol::CallSupport::Native } else { protocol::CallSupport::Handoff };
    row
}

fn said(row: &mut Conversation, from: &str, preview: &str) {
    row.preview = preview.into();
    if from == ME {
        row.preview_mine = true;
    } else if !row.kind.is_direct() || row.kind == ConversationKind::Group {
        row.preview_from = who(from).map_or(from, |who| who.name).split(' ').next().unwrap_or(from).into();
    }
}

fn message(id: &str, from: &str, text: &str, sent: i64) -> Message {
    let mut message = Message::plain(id, "", text, sent, from == ME);
    message.sender = sender(from);
    message
}

/// A styled run of `text`, found by its words.
fn span(text: &str, part: &str, style: Style) -> Span {
    let start = text.find(part).expect("the part is in the text");
    Span { start: start as u32, end: (start + part.len()) as u32, style }
}

fn reaction(emoji: &str, count: u32, mine: bool) -> Reaction {
    Reaction { emoji: emoji.into(), file: None, count, mine }
}

/// The conversation worth opening: four people planning a Saturday, over yesterday and today,
/// with one of everything a message can be.
fn carpool(key: &Key) -> Vec<Message> {
    let y = |hour, minute| on_day(1, hour, minute);
    let mut all = vec![
        message("m01", "ines", "Okay, Saturday foray is ON 🍄 Forecast says dry until 2pm.", y(19, 2)),
        message("m02", "ines", "Meeting at the Weir car park, 7:30. Bring boots, it'll be soggy by the stream.", y(19, 3)),
        message("m03", "tomasz", "I'm in. Can take 3 in the van if someone sorts snacks", y(19, 11)),
        message("m04", "yusuf", "Snacks: on it. Anyone allergic to anything?", y(19, 14)),
        message("m05", ME, "No allergies here. I'll bring the field guide and the spare loupe.", y(19, 20)),
        message("m06", ME, "Also the good thermos ☕", y(19, 21)),
        message(
            "m07",
            "ines",
            "Quick reminder of the plan so nobody gets lost like last time: we park at the Weir, walk the creek path up to the old fence line, then split into two groups. Group one does the birch stand on the east slope, group two takes the mossy bank by the second footbridge. We regroup at the picnic table around eleven, compare finds, and anyone who wants the afternoon loop can head up to the ridge with me. Please, please don't eat anything you can't identify with 100% certainty.",
            y(19, 34),
        ),
        message("m08", "tomasz", "Route notes are here: https://trailnotes.example/weir-creek", y(19, 40)),
        message("m09", "yusuf", "Morning! Picked up the snacks 🥐", ago(185)),
        message("m10", "yusuf", "Sunrise at the weir on the way past. Looks promising", ago(184)),
        message("m11", "ines", "Gorgeous 😍", ago(170)),
        message("m12", "tomasz", "For the spreadsheet nerds, here's the ID checklist script I mentioned:", ago(160)),
        message("m13", "tomasz", "Rule one:\ndef safe_to_pick(find):\n    return find.spore_print and find.gills != \"white\"", ago(159)),
        message("m14", ME, "Ha, love it. Adding a rule: if in doubt, leave it out.", ago(140)),
        message("m15", "ines", "Can you bring the second loupe too? Mine cracked.", ago(90)),
        message("m16", ME, "Yep, both loupes packed 👍", ago(85)),
        message("m17", "yusuf", "Here's the checklist as a PDF for anyone printing", ago(70)),
        message("m18", "tomasz", "Van's got a slow puncture, sorting it tonight. Shouldn't affect Saturday.", ago(50)),
        message("m19", "tomasz", "Worst case we take two cars.", ago(49)),
        message("m20", "ines", "If we need a second car, @Robin can you drive?", ago(30)),
        message("m21", ME, "Sure, I can take 4.", ago(25)),
        message("m22", "yusuf", "Perfect. That settles it then", ago(12)),
        message("m23", "ines", "See everyone at 7:30 sharp ⏰", ago(6)),
        message("m24", "tomasz", "🙌", ago(5)),
        message("m25", "ines", "Oh and bring a basket, not a plastic bag!", ago(4)),
    ];
    let at = |all: &mut Vec<Message>, id: &str| all.iter().position(|m| m.id == id).expect("a fixture message");
    let i = at(&mut all, "m04");
    all[i].reactions = vec![reaction("👍", 2, true)];
    let i = at(&mut all, "m06");
    all[i].reactions = vec![reaction("❤️", 3, false), reaction("☕", 1, false)];
    let i = at(&mut all, "m08");
    let url = "https://trailnotes.example/weir-creek";
    all[i].spans = vec![span(&all[i].text, url, Style::Link { url: url.into() })];
    all[i].previews = vec![Preview {
        url: url.into(),
        site: "Trail Notes".into(),
        title: "Weir Creek loop · 6.2 km".into(),
        description: "An easy loop along the creek, past two footbridges and the old fence line. Muddy after rain.".into(),
        image: Some(picture("preview-creek", MediaKind::Image, "preview-creek.webp", 640, 336)),
    }];
    let i = at(&mut all, "m10");
    all[i].attachments = vec![Attachment {
        mime: "image/webp".into(),
        ..Attachment::new("weir-dawn.webp", picture("weir-dawn", MediaKind::Image, "weir-dawn.webp", 640, 480))
    }];
    all[i].reactions = vec![reaction("😍", 2, false), reaction("🌅", 1, true)];
    let i = at(&mut all, "m13");
    let text = all[i].text.clone();
    let code = text.split_once('\n').map_or(text.as_str(), |(_, code)| code).to_owned();
    all[i].spans = vec![span(&text, &code, Style::CodeBlock { language: "python".into() })];
    let i = at(&mut all, "m14");
    all[i].edited = true;
    let i = at(&mut all, "m15");
    all[i].reply_to = Some(ReplyTo {
        id: "m05".into(),
        sender: ME_NAME.into(),
        snippet: "No allergies here. I'll bring the field guide and the spare loupe.".into(),
        person: Some(Key::new(&key.account(), ME)),
        deleted: false,
    });
    let i = at(&mut all, "m17");
    all[i].attachments = vec![Attachment {
        mime: "application/pdf".into(),
        ..Attachment::new(
            "Foray checklist.pdf",
            MediaFile { bytes: 241_664, ..MediaFile::new("foray-checklist", MediaKind::File) },
        )
    }];
    let i = at(&mut all, "m19");
    all[i].reactions = vec![reaction("😬", 1, false), reaction("🚗", 2, true)];
    let i = at(&mut all, "m20");
    let text = all[i].text.clone();
    all[i].spans = vec![span(&text, "@Robin", Style::Mention { kind: MentionKind::User, id: ME.into() })];
    all[i].mentions_me = true;
    all
}

/// A short history ending on the row's own preview, for every other conversation.
fn history(row: &Conversation, lines: &[(&str, &str, i64)]) -> Vec<Message> {
    lines
        .iter()
        .enumerate()
        .map(|(ix, (from, text, minutes))| {
            let sent = if *minutes < 0 { row.updated + minutes * 60 } else { row.updated - minutes * 60 };
            message(&format!("{}-{ix}", row.key.id), from, text, sent.min(row.updated))
        })
        .collect()
}

fn build() -> Fixture {
    let (t, d, s) = (tiny(), discord(), slack());
    if empty() {
        return Fixture { accounts: Vec::new(), spaces: Vec::new(), groups: Vec::new(), rows: Vec::new(), inbox: Vec::new(), messages: Vec::new() };
    }
    let moss = "moss-lichen";
    let harbor = "harbor-labs";
    let premier = "premier";

    let mut carpool_row = row(&d, "g-carpool", ConversationKind::Group, "Foray carpool 🍄", ago(4));
    carpool_row.avatar = Some(picture("group-carpool", MediaKind::Avatar, "group-foray-carpool.webp", 192, 192));
    carpool_row.unread = 4;
    carpool_row.last_read = Some("m21".into());
    carpool_row.call = protocol::CallSupport::Handoff;
    said(&mut carpool_row, "ines", "Oh and bring a basket, not a plastic bag!");

    let mut yusuf = direct(&d, "dm-yusuf", "yusuf", ago(22));
    yusuf.unread = 2;
    said(&mut yusuf, "yusuf", "sending the trail cam clips now 📹");

    let mut platform = in_space(row(&s, "c-platform", ConversationKind::Channel, "platform", ago(70)), harbor, "Harbor Labs", "eng", "Engineering", 0);
    platform.topic = "Infra, deploys and the on-call rota".into();
    platform.unread = 3;
    platform.mentions = 1;
    said(&mut platform, "mara", "@Robin the migration window moved to 9pm tonight");

    let mut priya = in_space(direct(&s, "dm-priya", "priya", ago(180)), harbor, "Harbor Labs", "directs", "Direct messages", 0);
    priya.seen = true;
    said(&mut priya, ME, "Sounds good, talk tomorrow!");

    let mut lobby = in_space(row(&t, "c-lobby", ConversationKind::Channel, "lobby", ago(300)), premier, "Premier", "rooms", "Rooms", 0);
    lobby.unread = 5;
    lobby.call = protocol::CallSupport::Native;
    said(&mut lobby, "halvard", "who's up for a call later?");

    let mut ana = direct(&t, "dm-ana", "ana", ago(400));
    ana.unread = 1;
    said(&mut ana, "ana", "Did the invite come through?");

    let mut lichen = in_space(row(&d, "th-lichen", ConversationKind::Thread, "Lichen ID help", on_day(1, 21, 15)), moss, "Moss & Lichen", "text", "Text channels", 2);
    lichen.parent = Some("c-general".into());
    lichen.root = Some("general-1".into());
    lichen.followed = true;
    lichen.unread = 2;
    lichen.mentions = 1;
    said(&mut lichen, "dita", "@Robin is this Cladonia or just very confused moss?");

    let mut cams = in_space(row(&d, "c-trailcams", ConversationKind::Channel, "trail-cams", on_day(1, 16, 48)), moss, "Moss & Lichen", "text", "Text channels", 3);
    cams.muted = true;
    cams.unread = 12;
    said(&mut cams, "kofi", "night shot of the marten again, third time this week");

    let mut imogen = in_space(direct(&s, "dm-imogen", "imogen", on_day(2, 15, 5)), harbor, "Harbor Labs", "directs", "Direct messages", 1);
    said(&mut imogen, "imogen", "Let me know when the deck is ready and I'll take a look before Thursday's review");

    let mut design = in_space(
        row(&s, "g-design", ConversationKind::Group, "Design crit — Q4 roadmap, onboarding flows and the offsite agenda", on_day(3, 11, 20)),
        harbor,
        "Harbor Labs",
        "directs",
        "Direct messages",
        2,
    );
    design.avatar = Some(picture("group-design", MediaKind::Avatar, "group-design-crit.webp", 192, 192));
    said(&mut design, "theo", "I moved the offsite agenda into the shared doc");

    let mut rosa = direct(&t, "dm-rosa", "rosa", on_day(4, 18, 30));
    rosa.title = "Grandma Rosa 🌻".into();
    said(&mut rosa, "rosa", "Call me when you're back from the woods 💛");

    let mut kofi = direct(&d, "dm-kofi", "kofi", on_day(5, 9, 12));
    said(&mut kofi, ME, "Thanks for the loan of the tripod!");

    let mut theo = in_space(direct(&s, "dm-theo", "theo", on_day(6, 17, 45)), harbor, "Harbor Labs", "directs", "Direct messages", 3);
    said(&mut theo, "theo", "🎉🎉🎉");

    let mut showcase = in_space(row(&d, "c-showcase", ConversationKind::Channel, "showcase", on_day(6, 8, 3)), moss, "Moss & Lichen", "text", "Text channels", 1);
    showcase.starred = true;
    said(&mut showcase, "ines", "Frost on the moss this morning");

    // Rows of the trees that the Inbox does not list.
    let mut general = in_space(row(&d, "c-general", ConversationKind::Channel, "general", on_day(1, 21, 10)), moss, "Moss & Lichen", "text", "Text channels", 0);
    said(&mut general, "dita", "Started a thread for the lichen question");
    let announcements = in_space(row(&d, "c-announcements", ConversationKind::Announcement, "announcements", on_day(9, 10, 0)), moss, "Moss & Lichen", "info", "Information", 0);
    let mut campfire = in_space(row(&d, "v-campfire", ConversationKind::Voice, "Campfire", on_day(2, 20, 0)), moss, "Moss & Lichen", "voice", "Voice", 0);
    campfire.occupants = Some(Occupants {
        people: ["yusuf", "ines"]
            .into_iter()
            .map(|id| Occupant { person: Key::new(&d, id), name: who(id).map_or(id, |w| w.name).into(), muted: id == "ines", deafened: false, streaming: false, speaking: id == "yusuf" })
            .collect(),
        total: 2,
    });
    let mut random = in_space(row(&s, "c-random", ConversationKind::Channel, "random", on_day(1, 12, 0)), harbor, "Harbor Labs", "eng", "Engineering", 1);
    said(&mut random, "theo", "who left the oat milk out");

    let inbox_rows = vec![
        carpool_row, yusuf, platform, priya, lobby, ana, lichen, cams, imogen, design, rosa, kofi, theo, showcase,
    ];
    let inbox: Vec<Key> = inbox_rows.iter().map(|row| row.key.clone()).collect();
    let mut rows = inbox_rows;
    rows.extend([general, announcements, campfire, random]);

    let mut messages = Vec::new();
    for row in &rows {
        let lines: Vec<(&str, &str, i64)> = match row.key.id.as_str() {
            "g-carpool" => {
                messages.push((row.key.clone(), carpool(&row.key)));
                continue;
            }
            "dm-yusuf" => vec![
                (ME, "Did the new trail cam batteries hold up?", 95),
                ("yusuf", "Like a charm. 4 nights and still at 60%", 60),
                ("yusuf", "Got the marten AND a fox on the same night", 59),
                (ME, "No way! Send them over", 40),
                ("yusuf", "sending the trail cam clips now 📹", 0),
            ],
            "c-platform" => vec![
                ("mara", "Heads up: Postgres 16 migration is scheduled for this week.", 240),
                ("theo", "Does that touch the design system preview env?", 200),
                ("mara", "Only for about ten minutes, I'll post in here before and after.", 190),
                ("mara", "@Robin the migration window moved to 9pm tonight", 0),
            ],
            "dm-priya" => vec![
                ("priya", "Have a sec to look at the onboarding flow tomorrow?", 30),
                (ME, "Yes! Morning works best for me", 20),
                ("priya", "10:30?", 10),
                (ME, "Sounds good, talk tomorrow!", 0),
            ],
            "c-lobby" => vec![
                ("ana", "Server's been rock solid since the update 👏", 220),
                ("halvard", "Uptime 41 days and counting", 200),
                ("ana", "We should do a game night", 120),
                ("halvard", "who's up for a call later?", 0),
            ],
            "dm-ana" => vec![(ME, "Sent you an invite to the Premier server", 600), ("ana", "Did the invite come through?", 0)],
            "th-lichen" => vec![
                ("dita", "Found this on the fence posts by the second footbridge.", 30),
                ("kofi", "Ooh, those little cups are lovely", 20),
                ("dita", "@Robin is this Cladonia or just very confused moss?", 0),
            ],
            "c-trailcams" => vec![
                ("yusuf", "Cam 3 moved to the oak by the creek", 600),
                ("kofi", "night shot of the marten again, third time this week", 0),
            ],
            "dm-imogen" => vec![
                ("imogen", "Great work on the roadmap draft.", 60),
                ("imogen", "Let me know when the deck is ready and I'll take a look before Thursday's review", 0),
            ],
            "g-design" => vec![
                ("priya", "Pushed new onboarding frames to the file", 90),
                ("theo", "Looks great. Small note on the empty states", 80),
                ("theo", "I moved the offsite agenda into the shared doc", 0),
            ],
            "dm-rosa" => vec![(ME, "Off to the woods this weekend!", 120), ("rosa", "Call me when you're back from the woods 💛", 0)],
            "dm-kofi" => vec![("kofi", "Tripod's on your doorstep", 300), (ME, "Thanks for the loan of the tripod!", 0)],
            "dm-theo" => vec![(ME, "We shipped it!", 5), ("theo", "🎉🎉🎉", 0)],
            "c-showcase" => vec![("ines", "Frost on the moss this morning", 0)],
            "c-general" => vec![("dita", "Started a thread for the lichen question", 0)],
            "c-random" => vec![("theo", "who left the oat milk out", 0)],
            _ => Vec::new(),
        };
        let mut said = history(row, &lines);
        if row.key.id == "c-showcase" {
            if let Some(last) = said.last_mut() {
                last.attachments = vec![Attachment::new("moss-frost.webp", picture("moss-frost", MediaKind::Image, "moss-frost.webp", 640, 480))];
                last.reactions = vec![reaction("❄️", 4, false), reaction("💚", 2, true)];
            }
        }
        if row.key.id == "th-lichen" {
            if let Some(last) = said.last_mut() {
                let text = last.text.clone();
                last.spans = vec![span(&text, "@Robin", Style::Mention { kind: MentionKind::User, id: ME.into() })];
                last.mentions_me = true;
            }
        }
        if row.key.id == "c-platform" {
            if let Some(last) = said.last_mut() {
                let text = last.text.clone();
                last.spans = vec![span(&text, "@Robin", Style::Mention { kind: MentionKind::User, id: ME.into() })];
                last.mentions_me = true;
            }
            // The migration notice has a thread under it, as Slack keeps the talk about it.
            if let Some(first) = said.first_mut() {
                let thread = Key { id: "c-platform:th-pg".into(), ..row.key.clone() };
                let replies = vec![
                    ("theo", "Will the preview env come back on its own after?", 235),
                    ("mara", "Yes, the deploy job restarts it. I'll check it by hand too.", 230),
                    (ME, "I'll keep an eye on the error rate while it runs", 228),
                    ("mara", "Thanks! I'll post the before and after numbers here.", 225),
                ];
                let in_thread: Vec<Message> = replies
                    .iter()
                    .enumerate()
                    .map(|(ix, (from, text, minutes))| message(&format!("th-pg-{ix}"), from, text, row.updated - minutes * 60))
                    .collect();
                first.thread = Some(protocol::ThreadSummary {
                    id: thread.id.clone(),
                    replies: replies.len() as u32,
                    last: row.updated - 225 * 60,
                    participants: ["mara", ME, "theo"].into_iter().map(|id| Key { id: id.into(), ..row.key.clone() }).collect(),
                    unread: 0,
                });
                messages.push((thread, in_thread));
            }
        }
        messages.push((row.key.clone(), said));
    }

    let space = |account: &AccountKey, id: &str, name: &str, icon: Option<MediaFile>, position: i32, attention: u32| Space {
        key: Key::new(account, id),
        name: name.into(),
        icon,
        position,
        attention,
        unread: true,
        folder: None,
        muted: false,
        hide_muted: false,
    };
    let spaces = vec![
        space(&t, premier, "Premier", Some(picture("icon-premier", MediaKind::Icon, "icon-night-shift.webp", 192, 192)), 0, 0),
        space(&d, moss, "Moss & Lichen", Some(picture("icon-moss", MediaKind::Icon, "icon-moss-lichen.webp", 192, 192)), 0, 1),
        space(&s, harbor, "Harbor Labs", Some(picture("icon-harbor", MediaKind::Icon, "logo-harbor-labs.png", 192, 192)), 0, 1),
    ];
    let directs = |name: &str| Group { kind: GroupKind::Directs, ..Group::new("directs", name, 9) };
    let groups = vec![
        (Key::new(&t, premier), vec![Group::new("rooms", "Rooms", 0)]),
        (Key::new(&d, moss), vec![Group::new("info", "Information", 0), Group::new("text", "Text channels", 1), Group::new("voice", "Voice", 2)]),
        (Key::new(&s, harbor), vec![Group::new("eng", "Engineering", 0), directs("Direct messages")]),
    ];
    let accounts = vec![
        // Discord first: an account that is a person names me on You and the tab bar.
        account(d.clone(), ME_NAME, 3, true, Presence::Online),
        account(s.clone(), "Harbor Labs", 1, true, Presence::Idle),
        account(t.clone(), "Premier", 1, true, Presence::Online),
    ];
    Fixture { accounts, spaces, groups, rows, inbox, messages }
}

// Answers.

fn networks(fixture: &Fixture) -> Vec<NetworkStatus> {
    protocol::Network::ALL
        .into_iter()
        .map(|network| NetworkStatus {
            network,
            link: if fixture.accounts.iter().any(|a| a.key.network == network) { Link::Connected } else { Link::SignedOut },
        })
        .collect()
}

fn find(key: &Key) -> Option<&'static Conversation> {
    fixture().rows.iter().find(|row| &row.key == key)
}

fn messages_of(key: &Key) -> &'static [Message] {
    fixture().messages.iter().find(|(k, _)| k == key).map_or(&[], |(_, m)| m.as_slice())
}

fn conversations(rows: Vec<Conversation>, offset: u32, limit: u32) -> Response {
    let total = rows.len() as u32;
    let counts = InboxCounts {
        all: total,
        unread: rows.iter().filter(|r| r.unread > 0 && !r.muted).count() as u32,
        mentions: rows.iter().filter(|r| r.mentions > 0).count() as u32,
        apps: 0,
    };
    let page = rows.into_iter().skip(offset as usize).take(limit.max(1) as usize).collect();
    Response::Conversations { conversations: page, total, counts }
}

fn inbox(network: Option<Network>, filter: InboxFilter) -> Vec<Conversation> {
    let fixture = fixture();
    let mut rows: Vec<Conversation> = fixture
        .inbox
        .iter()
        .filter_map(find)
        .filter(|row| network.is_none_or(|n| row.key.network == n))
        .filter(|row| match filter {
            InboxFilter::All => true,
            InboxFilter::Unread => (row.unread > 0 && !row.muted) || row.mentions > 0,
            InboxFilter::Mentions => row.mentions > 0,
            InboxFilter::Directs => row.kind.is_direct(),
        })
        .cloned()
        .collect();
    rows.sort_by(|a, b| b.starred.cmp(&a.starred).then(b.updated.cmp(&a.updated)));
    rows
}

fn page(key: &Key, page: &Page, limit: u32) -> Response {
    let all = messages_of(key);
    let limit = limit.max(1) as usize;
    let (from, to) = match page {
        Page::Newest => (all.len().saturating_sub(limit), all.len()),
        Page::Before { id } => {
            let end = all.iter().position(|m| &m.id == id).unwrap_or(0);
            (end.saturating_sub(limit), end)
        }
        Page::After { id } => {
            let start = all.iter().position(|m| &m.id == id).map_or(all.len(), |at| at + 1);
            (start, (start + limit).min(all.len()))
        }
        Page::Around { id } => {
            let at = all.iter().position(|m| &m.id == id).unwrap_or(0);
            let start = at.saturating_sub(limit / 2);
            (start, (start + limit).min(all.len()))
        }
    };
    Response::Messages {
        conversation: key.clone(),
        messages: all[from..to].to_vec(),
        last_read: find(key).and_then(|row| row.last_read.clone()).or_else(|| all.last().map(|m| m.id.clone())),
        older: from > 0,
        newer: to < all.len(),
    }
}

fn members(key: &Key) -> Vec<Person> {
    let mut ids: Vec<&str> = Vec::new();
    for message in messages_of(key) {
        if message.sender.id != ME && !ids.contains(&message.sender.id.as_str()) {
            ids.push(&message.sender.id);
        }
    }
    if ids.is_empty() || !find(key).is_some_and(|row| row.kind.is_direct()) {
        let more: Vec<&str> = PEOPLE.iter().filter(|w| w.network == key.network && !ids.contains(&w.id)).map(|w| w.id).collect();
        ids.extend(more);
    }
    let mut people: Vec<Person> = ids.into_iter().filter_map(who).map(person_of).collect();
    people.insert(0, me_on(&key.account()));
    people
}

fn profile(person: &Key) -> Response {
    let base = if person.id == ME { me_on(&person.account()) } else { who(&person.id).map(person_of).unwrap_or_else(|| Person::new(person.clone(), "Someone")) };
    let mut profile = Profile::of(base);
    match person.id.as_str() {
        "yusuf" => {
            profile.bio = "Trail cams, foxes and far too many batteries. Weekend forager.".into();
            profile.pronouns = "he/him".into();
            profile.mutual_spaces = vec!["Moss & Lichen".into(), "Night Shift".into()];
            profile.mutual_friends = 6;
            profile.accent = Some(0x2f6f4f);
            profile.time_zone = "Europe/Istanbul".into();
            profile.utc_offset = Some(3 * 3600);
            profile.since = Some(on_day(1_200, 12, 0));
        }
        "priya" | "mara" | "imogen" | "theo" => {
            profile.title = who(&person.id).map_or("", |w| w.title).into();
            profile.department = "Product & Engineering".into();
            profile.time_zone = "Europe/London".into();
            profile.utc_offset = Some(0);
        }
        _ => {
            profile.bio = "Out in the woods most weekends.".into();
            profile.mutual_spaces = vec!["Moss & Lichen".into()];
        }
    }
    Response::Profile { profile: Box::new(profile) }
}

fn hits(query: &str, only: Option<&Key>) -> Vec<MessageHit> {
    let needle = query.to_lowercase();
    let fixture = fixture();
    let mut out: Vec<MessageHit> = fixture
        .messages
        .iter()
        .filter(|(key, _)| only.is_none_or(|only| only == key))
        .flat_map(|(key, all)| {
            let row = find(key);
            all.iter().filter(|m| !needle.is_empty() && m.text.to_lowercase().contains(&needle)).map(move |m| MessageHit {
                conversation: key.clone(),
                message_id: m.id.clone(),
                sent: m.sent,
                sender: m.sender.name.clone(),
                snippet: m.text.replace('\n', " "),
                title: row.map(|r| r.title.clone()).unwrap_or_default(),
                space_title: row.map(|r| r.space_title.clone()).unwrap_or_default(),
                outgoing: m.outgoing,
            })
        })
        .collect();
    out.sort_by(|a, b| b.sent.cmp(&a.sent));
    out
}

/// The Tiny server "Premier", as its owner sees it: three people, two rooms, one open invite.
fn premier() -> protocol::TinyCommunity {
    let member = |user_id: &str, name: &str, email: &str, role: &str, days: i64| protocol::TinyMember {
        user_id: user_id.into(),
        email: email.into(),
        display_name: name.into(),
        role: role.into(),
        created_at: stamp(on_day(days, 12, 0)),
        ..Default::default()
    };
    protocol::TinyCommunity {
        role: "owner".into(),
        members: vec![
            member(ME, ME_NAME, "robin@example.com", "owner", 120),
            member("halvard", "Halvard Eide", "halvard@example.com", "admin", 90),
            member("ana", "Ana Ruiz", "", "member", 12),
        ],
        invites: vec![protocol::TinyInvite {
            id: "inv-1".into(),
            role: "member".into(),
            created_by: ME.into(),
            expires_at: stamp(now() + 5 * 86_400),
            accepted_at: None,
            accepted_by: None,
            ..Default::default()
        }],
        audit: Vec::new(),
        origin: "https://premier.example".into(),
        rooms: vec![
            protocol::TinyRoom::new("c-lobby", "lobby"),
            protocol::TinyRoom {
                participants: vec![protocol::TinySeat { user_id: "halvard".into(), ..Default::default() }],
                ..protocol::TinyRoom::new("r-games", "game night")
            },
        ],
        online: vec![ME.into(), "halvard".into()],
        media: "up".into(),
        capabilities: Default::default(),
        access: None,
        me: ME.into(),
        server: None,
        roles: Vec::new(),
        categories: Vec::new(),
        bans: Vec::new(),
        provider: String::new(),
    }
}

/// Unix seconds as the server writes them: RFC 3339, UTC.
fn stamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds / 3600, seconds % 3600 / 60, seconds % 60)
}

/// What the made-up chatd says to one request.
pub fn answer(request: &Request) -> std::io::Result<Response> {
    let fixture = fixture();
    let answer = match request {
        Request::Ping => Response::Pong,
        Request::Status => Response::Status {
            networks: networks(fixture),
            accounts: fixture.accounts.clone(),
            media: Media::Stopped,
            call: None,
            seq: 1,
            keychain: protocol::Keychain::default(),
            started: 1,
            // No session: a shot starts nothing that would share it.
            session: String::new(),
        },
        // Nothing ever changes: the wait runs out and is asked again, as a quiet chatd's does.
        Request::Wait { .. } => {
            std::thread::sleep(Duration::from_secs(20));
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the demo is quiet"));
        }
        Request::Inbox { network, filter, offset, limit } => conversations(inbox(*network, *filter), *offset, *limit),
        Request::Spaces { network } => Response::Spaces {
            spaces: fixture.spaces.iter().filter(|s| network.is_none_or(|n| s.key.network == n)).cloned().collect(),
        },
        Request::Tree { space, offset, limit, .. } => {
            let groups = fixture.groups.iter().find(|(k, _)| k == space).map(|(_, g)| g.clone()).unwrap_or_default();
            let mut rows: Vec<Conversation> = fixture
                .rows
                .iter()
                .filter(|row| row.key.account() == space.account() && row.space.as_deref() == Some(space.id.as_str()))
                .cloned()
                .collect();
            rows.sort_by_key(|row| {
                let group = groups.iter().position(|g| Some(&g.id) == row.group.as_ref()).unwrap_or(99);
                (group, row.position)
            });
            let groups = groups
                .into_iter()
                .map(|g| Group { count: rows.iter().filter(|r| r.group.as_ref() == Some(&g.id)).count() as u32, ..g })
                .collect();
            let total = rows.len() as u32;
            Response::Tree {
                space: space.clone(),
                groups,
                conversations: rows.into_iter().skip(*offset as usize).take(*limit as usize).collect(),
                total,
            }
        }
        Request::Conversation { conversation } => match find(conversation) {
            Some(row) => Response::Conversation { conversation: row.clone() },
            None => Response::Error { code: protocol::ErrorCode::NotFound, detail: "Not in the demo.".into() },
        },
        Request::Find { network, query, limit } => {
            let needle = query.to_lowercase();
            let rows = fixture
                .rows
                .iter()
                .filter(|row| network.is_none_or(|n| row.key.network == n) && row.title.to_lowercase().contains(&needle))
                .cloned()
                .collect();
            conversations(rows, 0, *limit)
        }
        Request::Home { network, offset, limit, .. } => {
            let rows = inbox(Some(*network), InboxFilter::All).into_iter().filter(|row| row.space.is_none()).collect();
            conversations(rows, *offset, *limit)
        }
        Request::Threads { space, offset, limit } => {
            let rows = fixture.rows.iter().filter(|r| r.kind == ConversationKind::Thread && r.space.as_deref() == Some(space.id.as_str())).cloned().collect();
            conversations(rows, *offset, *limit)
        }
        Request::Topics { .. } | Request::InVoice { .. } => conversations(Vec::new(), 0, 1),
        Request::Folders { .. } => Response::Folders { folders: Vec::new() },
        Request::SearchMessages { conversation, query, limit, .. } => {
            Response::MessageHits { hits: hits(query, conversation.as_ref()).into_iter().take(*limit as usize).collect() }
        }
        Request::SearchNetwork { conversation, query, .. } => Response::MessageHits { hits: hits(query, conversation.as_ref()) },
        Request::Messages { conversation, page: at, limit } => page(conversation, at, *limit),
        Request::Pinned { conversation } => Response::Pinned {
            conversation: conversation.clone(),
            messages: messages_of(conversation).iter().filter(|m| m.id == "m07").cloned().collect(),
        },
        Request::Shared { conversation, .. } => Response::Shared { conversation: conversation.clone(), items: Vec::new(), more: false, complete: true, total: Some(0) },
        Request::Reactions { conversation } => Response::Reactions {
            conversation: conversation.clone(),
            rules: serde_json::from_value(serde_json::json!({})).expect("the default rules"),
        },
        Request::Catalog { .. } => Response::Catalog { items: Vec::new(), cursor: None },
        Request::Watch(watching) => {
            let people = watching
                .people
                .iter()
                .filter_map(|key| if key.id == ME { Some(me_on(&key.account())) } else { who(&key.id).map(person_of) })
                .collect();
            Response::Watching { people, typists: Vec::new() }
        }
        Request::People { network, query, offset, limit, .. } => {
            let needle = query.as_deref().unwrap_or("").to_lowercase();
            let people: Vec<Person> = PEOPLE
                .iter()
                .filter(|w| network.is_none_or(|n| w.network == n) && w.name.to_lowercase().contains(&needle))
                .map(person_of)
                .collect();
            let total = people.len() as u32;
            Response::People { people: people.into_iter().skip(*offset as usize).take(*limit as usize).collect(), total }
        }
        Request::Around { network } => {
            let mine = |w: &&Who| network.is_none_or(|n| w.network == n);
            Response::Around {
                requests: 0,
                in_call: Vec::new(),
                online: PEOPLE.iter().filter(mine).filter(|w| w.presence.is_around()).map(person_of).collect(),
                recent: PEOPLE.iter().filter(mine).filter(|w| !w.presence.is_around()).map(person_of).collect(),
            }
        }
        Request::Members { conversation, offset, .. } => {
            let people = members(conversation);
            let online = people.iter().filter(|p| p.presence.is_around()).count() as u32;
            Response::Members {
                pending: false,
                conversation: conversation.clone(),
                sections: vec![
                    MemberSection { name: "Online".into(), count: online },
                    MemberSection { name: "Offline".into(), count: people.len() as u32 - online },
                ],
                offset: *offset,
                members: {
                    let mut sorted = people;
                    sorted.sort_by_key(|p| !p.presence.is_around());
                    sorted
                },
            }
        }
        Request::Profile { person, .. } => profile(person),
        Request::ConversationInfo { conversation } => match find(conversation) {
            Some(row) => Response::ConversationInfo {
                info: Box::new(ConversationInfo {
                    conversation: row.clone(),
                    description: row.topic.clone(),
                    members: Some(members(conversation).len() as u32),
                    online: Some(members(conversation).iter().filter(|p| p.presence.is_around()).count() as u32),
                    created: Some(on_day(400, 10, 0)),
                    creator: "Ines Marchetti".into(),
                    notify: if row.muted { Notify::Nothing } else { Notify::All },
                    pinned: Some(1),
                    external: String::new(),
                    link: String::new(),
                }),
            },
            None => Response::Error { code: protocol::ErrorCode::NotFound, detail: "Not in the demo.".into() },
        },
        Request::SpaceInfo { space } => match fixture.spaces.iter().find(|s| &s.key == space) {
            Some(found) => Response::SpaceInfo {
                info: Box::new(SpaceInfo {
                    space: found.clone(),
                    description: "A friendly corner for foragers, lichen nerds and trail-cam watchers.".into(),
                    members: Some(1_284),
                    online: Some(212),
                    nickname: String::new(),
                    roles: vec!["Member".into()],
                    notify: Notify::Mentions,
                }),
            },
            None => Response::Error { code: protocol::ErrorCode::NotFound, detail: "Not in the demo.".into() },
        },
        Request::Fetch { .. } => Response::Media { files: Vec::new() },
        Request::Settings => Response::Settings { values: Vec::new() },
        Request::Storage => Response::Storage { messages: 48_213, files: 734_003_200 },
        Request::Prunable { .. } => Response::Prunable { messages: 0, bytes: 0 },
        Request::SignIn { .. } => Response::Error { code: protocol::ErrorCode::SignIn, detail: "The sign-in window was closed.".into() },
        Request::TinyCommunity { .. } => Response::TinyCommunity { community: premier() },
        Request::SignInBegin { .. }
        | Request::Handoff { .. }
        | Request::CallStart { .. }
        | Request::Media(_) => Response::Error { code: protocol::ErrorCode::Unsupported, detail: "Not in the demo.".into() },
        // Everything that would change something: taken, and forgotten.
        _ => Response::Ok,
    };
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Badge;

    #[test]
    fn the_demo_has_its_shape() {
        let fixture = build();
        assert_eq!(fixture.accounts.len(), 3);
        assert_eq!(fixture.inbox.len(), 14);
        let mentions = fixture.rows.iter().filter(|r| r.mentions > 0).count();
        assert_eq!(mentions, 2);
        assert_eq!(fixture.rows.iter().filter(|r| fixture.inbox.contains(&r.key) && r.muted).count(), 1);
        let carpool = fixture.messages.iter().find(|(k, _)| k.id == "g-carpool").unwrap();
        assert!(carpool.1.len() >= 25);
        assert!(fixture.rows.iter().any(|r| r.badge() == Badge::Dot));
    }
}
