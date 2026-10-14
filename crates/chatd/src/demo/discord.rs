//! The demo's Discord account: Robin Vale, eight servers (fifty with `many`), friends, direct
//! conversations.

use super::png::Art;
use super::{Fallible, Listed, Seeder, Who};
use crate::store::{ConversationRow, SpaceRow};
use protocol::{
    AccountKey, CallSupport, ConversationKind, Group, Key, MediaKind, Network, Occupant, Occupants,
    Presence, Relation,
};
use ConversationKind::{Announcement, Channel, Forum, Thread, Voice};
use Presence::{Dnd, Idle, Offline, Online, Unknown};
use Relation::Friend;

pub const ME: &str = "210000000000000000";
pub const MOSS: &str = "300000000000000001";
pub const SHOWCASE: &str = "400000000000000007";
pub const BUILD_LOG: &str = "400000000000000008";
pub const CAMPFIRE: &str = "400000000000000035";

pub fn account() -> AccountKey {
    AccountKey {
        network: Network::Discord,
        account: ME.to_owned(),
    }
}

/// What the rest of the demo needs of this account.
pub(crate) struct Discord {
    pub account: AccountKey,
    pub me: Who,
    pub cast: Vec<Who>,
    /// The direct conversation `live` writes into.
    pub live: Key,
    /// The voice channel `live` changes the occupants of.
    pub voice: Key,
}

const PEOPLE: &[Listed] = &[
    ("Ines Marchetti", "ines.m", Online, "sketching trail maps", Friend),
    ("Tomasz Brandt", "tbrandt", Online, "", Friend),
    ("Wren Halloran", "wrenh", Idle, "back in 20", Friend),
    ("Odalys Ferreira", "odalys", Dnd, "deep work until 3", Friend),
    ("Kofi Asante", "kofi.a", Offline, "", Friend),
    ("Mirela Dragan", "mireladragan", Online, "🎧 synthwave", Friend),
    ("Haruto Senda", "haru", Idle, "", Friend),
    ("Bex Lindqvist", "bexl", Offline, "on a train", Friend),
    ("Anselm Okafor", "anselm", Unknown, "", Friend),
    ("Dita Novak", "ditanovak", Online, "", Friend),
    ("Rafael Quintero", "rafaq", Dnd, "in a meeting", Friend),
    ("Saoirse Whelan", "saoirse", Offline, "", Friend),
    ("Yusuf Demirci", "yusufd", Online, "shipping the thing", Friend),
    ("Pilar Echeverria", "pilar.e", Idle, "", Friend),
    ("Greta Sundqvist", "gretas", Unknown, "", Friend),
    ("Emeka Nwosu", "emeka", Online, "", Friend),
    ("Liang Qiu", "liangq", Offline, "", Friend),
    ("Marit Aasen", "marit", Online, "☕", Friend),
    ("Jonas Feldt", "jfeldt", Dnd, "", Friend),
    ("Nadia Rahimi", "nadiar", Idle, "walking the dog", Friend),
    ("Teodor Ilves", "teodor", Offline, "", Friend),
    ("Calla Winterbourne", "calla", Online, "", Friend),
    ("Ravi Subramanian", "ravi.s", Unknown, "", Friend),
    ("Halvard Eide", "halvard", Offline, "gone fishing, literally", Friend),
    ("Suvi Laine", "suvi", Online, "", Friend),
    ("Dorian Pask", "dorianp", Unknown, "", Relation::Incoming),
    ("Ottilie Brandvold", "ottilie", Unknown, "", Relation::Outgoing),
    ("Vex Morrow", "vexm", Unknown, "", Relation::Blocked),
    ("Casimir Thorne", "casimir", Unknown, "", Relation::None),
    ("Lichen Bot", "lichenbot", Unknown, "", Relation::None),
];

/// Indexes into [`PEOPLE`] for the people the scripts name.
pub const INES: usize = 0;
pub const TOMASZ: usize = 1;
pub const WREN: usize = 2;
pub const ODALYS: usize = 3;
pub const KOFI: usize = 4;
pub const MIRELA: usize = 5;
pub const HARUTO: usize = 6;
pub const DITA: usize = 9;
pub const YUSUF: usize = 12;
pub const BOT: usize = 29;
/// Past the end of the cast: us.
pub const YOU: usize = usize::MAX;

/// Moss & Lichen, top to bottom: a category, then its channels as (name, kind, unread,
/// mentions, muted).
type Row = (&'static str, ConversationKind, u32, u32, bool);
const MOSS_TREE: &[(&str, &[Row])] = &[
    ("", &[("rules", Announcement, 0, 0, false), ("announcements", Announcement, 2, 0, false)]),
    ("Start here", &[
        ("welcome", Channel, 0, 0, false),
        ("introductions", Channel, 0, 0, false),
        ("roles", Channel, 0, 0, true),
    ]),
    ("Trail", &[
        ("general", Channel, 5, 2, false),
        ("showcase", Channel, 0, 0, false),
        ("Trail camera build log", Thread, 0, 0, false),
        ("trail-cams", Channel, 5, 0, false),
        ("field-notes", Channel, 0, 0, false),
        ("route-planning", Channel, 9, 0, true),
        ("gear-swap", Channel, 6, 1, true),
        ("photo-dump", Channel, 0, 0, false),
    ]),
    ("Identification", &[
        ("mosses", Channel, 0, 0, false),
        ("lichens", Channel, 1, 0, false),
        ("fungi", Channel, 0, 0, false),
        ("ferns", Channel, 0, 0, false),
        ("mystery-specimens", Channel, 3, 0, false),
        ("id-help", Forum, 0, 0, false),
    ]),
    ("Projects", &[
        ("herbarium-scans", Channel, 0, 0, false),
        ("microscope-club", Channel, 0, 0, false),
        ("map-layers", Channel, 1, 1, false),
        ("citizen-science", Channel, 0, 0, false),
        ("grant-writing", Channel, 0, 0, true),
    ]),
    ("Meetups", &[
        ("spring-foray", Channel, 4, 0, false),
        ("autumn-foray", Channel, 0, 0, false),
        ("carpools", Channel, 0, 0, false),
        ("lodging", Channel, 0, 0, false),
    ]),
    ("Off trail", &[
        ("off-topic", Channel, 0, 0, false),
        ("cooking", Channel, 0, 0, false),
        ("music", Channel, 0, 0, true),
        ("pets", Channel, 1, 0, false),
        ("books", Channel, 0, 0, false),
        ("games", Channel, 0, 0, false),
    ]),
    ("Voice", &[
        ("Campfire", Voice, 0, 0, false),
        ("Field recording", Voice, 0, 0, false),
        ("Quiet room", Voice, 0, 0, false),
    ]),
    ("Staff", &[
        ("mod-log", Channel, 0, 0, true),
        ("mod-chat", Channel, 0, 0, false),
        ("bot-commands", Channel, 0, 0, false),
    ]),
];

/// The other servers: name, then categories of text channels.
type Small = (&'static str, &'static [(&'static str, &'static [&'static str])]);
const SMALL: &[Small] = &[
    ("Rustaceans Anonymous", &[
        ("Talk", &["general", "beginners", "show-and-tell"]),
        ("Deep ends", &["async", "embedded", "unsafe-corner"]),
    ]),
    ("Synth DIY", &[
        ("Bench", &["general", "build-logs", "troubleshooting"]),
        ("Market", &["wanted", "for-sale"]),
    ]),
    ("Pixel Foundry", &[
        ("Studio", &["general", "work-in-progress", "critique", "palettes"]),
    ]),
    ("Night Shift Games", &[
        ("Lobby", &["general", "looking-for-group"]),
        ("Games", &["co-op", "strategy", "clips"]),
    ]),
    ("Tiny Tools", &[
        ("Workshop", &["general", "releases", "feedback"]),
    ]),
    // Names that do not fit: one far too long for the sidebar's header, one with emoji in it.
    ("The Society for the Preservation of Unreasonably Long Server Names and Their Abbreviations", &[
        ("Proceedings", &["general", "minutes-of-the-last-meeting", "points-of-order"]),
    ]),
    ("🌋 Volcano Club 🔥", &[
        ("Crater", &["general", "eruptions", "🌡️-readings"]),
    ]),
];

/// What a channel of Moss & Lichen is for, where its owners said.
fn topic_of(name: &str) -> &'static str {
    match name {
        "general" => "Trail talk, plans and frost reports. Photos go in photo-dump.",
        "showcase" => "One of everything a message can hold.",
        "trail-cams" => "Camera status and what they caught overnight",
        "rules" => "Read these first",
        "id-help" => "One post per specimen, with a scale in the photo",
        _ => "",
    }
}

/// Who is in each voice channel: cast members with (muted, deafened, streaming), and how many
/// are in it in all. The last one holds more people than a row may name.
type Seat = (usize, bool, bool, bool);
const VOICES: &[(&str, &[Seat], u32)] = &[
    ("Campfire", &[(INES, false, false, false), (TOMASZ, true, false, false), (WREN, false, false, true)], 3),
    ("Field recording", &[(YUSUF, true, true, false)], 1),
    ("Quiet room", &[], 31),
];

/// Words for the names of the servers `many` adds.
const FIRST: &[&str] = &["Quiet", "Paper", "Midnight", "Copper", "Salt", "Glass", "Northern"];
const SECOND: &[&str] = &["Kettle", "Harbour", "Orchard", "Signal", "Lantern", "Archive"];

const GENERAL: &[(usize, i64, &str)] = &[
    (INES, 3200, "Morning all. Frost on the north slope, so the cushion mosses are finally showing their capsules."),
    (TOMASZ, 3190, "Bringing the macro lens on Saturday then."),
    (WREN, 3110, "Does anyone have a spare hand lens? Mine went into the river last week."),
    (HARUTO, 3105, "I have two. One is yours if you carry the tripod."),
    (WREN, 3104, "Deal."),
    (DITA, 1620, "The county finally published the new trail closures. Ridge loop is shut until April."),
    (INES, 1611, "That is the one with the old-growth stand. Shame."),
    (YOU, 1600, "We could do the creek route instead. Wetter, but more liverworts than anywhere else nearby."),
    (TOMASZ, 1598, "Creek route has my vote."),
    (KOFI, 1590, "Mine too, as long as someone brings a thermos."),
    (MIRELA, 420, "Scanned forty herbarium sheets last night. The 1911 ones are in better shape than the 1970s ones."),
    (ODALYS, 415, "Better paper, worse handwriting."),
    (MIRELA, 414, "So much worse handwriting."),
    (HARUTO, 190, "Reminder that the microscope club meets Thursday. Bring slides, not opinions."),
    (INES, 62, "@Robin Vale are you still bringing the trail camera on Saturday?"),
    (TOMASZ, 55, "Also the spare batteries, the last set died by noon."),
    (DITA, 31, "Carpool list is pinned in the meetups channel."),
    (WREN, 12, "Found something odd on a fence post. Photos in mystery-specimens."),
    (INES, 4, "@Robin Vale need a yes or no on the camera before I book the van."),
];

/// A long, plausible history for paging: lines are put together from parts.
const OPENERS: &[&str] = &[
    "Has anyone tried", "I finally got around to", "Small win today:", "Question about",
    "Reading the docs on", "Benchmarks are in for", "Still stuck on", "Hot take about",
    "Rewrote", "Found a bug in", "Thinking out loud about", "Follow-up on",
];
const TOPICS: &[&str] = &[
    "pinning in async traits", "the borrow checker and self-referential structs",
    "arena allocation for the parser", "const generics in the matrix code",
    "the embedded HAL on the nRF board", "error types that stay small",
    "an allocation-free tokenizer", "channels versus a mutex around the queue",
    "cross-compiling for the Pi Zero", "the lifetime elision rules",
    "trait objects in the plugin loader", "SIMD in the checksum loop",
];
const CLOSERS: &[&str] = &[
    "It compiles, which proves nothing.", "Halved the allocations.", "Will write it up later.",
    "Am I holding it wrong?", "The compiler was right again.", "Twelve lines shorter now.",
    "Opinions welcome.", "Flame graph attached in spirit.", "Clippy had thoughts.", "",
];

fn moss(s: &mut Seeder, account: &AccountKey, cast: &[Who], me: &Who) -> Fallible<()> {
    let space = Key::new(account, MOSS);
    let mut groups = Vec::new();
    let mut rows = Vec::new();
    let mut state = Vec::new();
    for (at, (category, channels)) in MOSS_TREE.iter().enumerate() {
        let group = (!category.is_empty()).then(|| format!("350000000000000{at:03}"));
        if let Some(id) = &group {
            // The staff category is muted, so it starts collapsed.
            groups.push(Group { muted: *category == "Staff", ..Group::new(id.clone(), *category, rows.len() as i32) });
        }
        for (name, kind, unread, mentions, muted) in *channels {
            let id = format!("400000000000000{:03}", rows.len() + 1);
            state.push((id.clone(), *unread, *mentions, *muted));
            rows.push(ConversationRow {
                group: group.clone(),
                position: rows.len() as i32,
                call: if *kind == Voice { CallSupport::Native } else { CallSupport::None },
                // The showcase is in the Inbox too, so it is one click away.
                listed: *name == "showcase",
                topic: topic_of(name).to_owned(),
                // Only staff post in these, and only staff see the staff channels.
                read_only: *kind == Announcement,
                private: *category == "Staff",
                ..ConversationRow::new(id, *kind, *name)
            });
        }
    }
    s.store.replace_tree(&space, &groups, &rows)?;
    let id_of = |name: &str| {
        let row = rows.iter().find(|row| row.title == name).expect("a channel of the tree");
        Key::new(account, row.id.clone())
    };

    let general = s.script(&id_of("general"), cast, me, GENERAL)?;
    let announced = s.script(&id_of("announcements"), cast, me, &[
        (INES, 9000, "Welcome to the new members from the county naturalists' list. Read the rules, then say hello in introductions."),
        (INES, 2900, "Spring foray is set for the second weekend of April. Sign-up sheet goes out Monday."),
        (INES, 130, "Sign-up sheet is live. Twenty places, lodging for twelve."),
    ])?;
    s.script(&id_of("trail-cams"), cast, me, &[
        (TOMASZ, 900, "Camera 2 caught a pine marten at 03:12. First one on this ridge in years."),
        (WREN, 880, "That tail! Send the clip to the county recorder."),
        (TOMASZ, 300, "Battery on camera 3 is at 11%. Who is up there next?"),
        (HARUTO, 290, "I am, Thursday. I will swap it."),
        (TOMASZ, 45, "Camera 1 has gone quiet. Either the card is full or a deer ate it again."),
    ])?;
    s.script(&id_of("mystery-specimens"), cast, me, &[
        (WREN, 14, "Fence post, north side, about a metre up. Grey-green, powdery edges."),
        (ODALYS, 11, "Lepraria, probably. Do you have a closer shot?"),
        (WREN, 9, "Phone would not focus. Going back tomorrow with the hand lens."),
    ])?;
    s.script(&id_of("gear-swap"), cast, me, &[
        (KOFI, 700, "Selling a barely used field press. Too big for my flat."),
        (DITA, 640, "Robin was after one of those, I think?"),
    ])?;
    s.script(&id_of("map-layers"), cast, me, &[
        (HARUTO, 150, "@Robin Vale the closures layer is ready for you to check against the county map."),
    ])?;
    s.script(&id_of("route-planning"), cast, me, &[
        (DITA, 500, "Creek route is 11 km with the detour. Four hours at foray pace."),
        (HARUTO, 480, "Foray pace being one kilometre an hour and a lot of kneeling."),
    ])?;
    for (channel, seats, total) in VOICES {
        let seated = |&(at, muted, deafened, streaming): &Seat| Occupant {
            person: Key::new(account, cast[at].id.clone()),
            name: cast[at].name.clone(),
            muted,
            deafened,
            streaming,
        };
        let mut people: Vec<Occupant> = seats.iter().map(seated).collect();
        // A full room: everyone we know who is not already talking somewhere (a person is in
        // one voice channel at a time, and Odalys is in a call), and more than a row may name.
        let busy = |at: usize| at == ODALYS || VOICES.iter().any(|(_, seats, _)| seats.iter().any(|seat| seat.0 == at));
        let free = (0..cast.len()).filter(|at| !busy(*at)).take((*total as usize).saturating_sub(people.len()));
        people.extend(free.map(|at| seated(&(at, at % 3 == 0, false, false))));
        s.occupants.push((id_of(channel), Occupants { people, total: *total }));
    }

    for (id, unread, mentions, muted) in state {
        let conversation = Key::new(account, id);
        let read = match conversation.id.as_str() {
            id if id == id_of("general").id => Some(general[13].as_str()),
            id if id == id_of("announcements").id => Some(announced[1].as_str()),
            _ => None,
        };
        s.counts(&conversation, unread, mentions, read)?;
        s.store.set_muted(&conversation, muted)?;
    }
    Ok(())
}

/// The other five servers. The first one's `#general` is the long history.
fn small(s: &mut Seeder, account: &AccountKey, cast: &[Who]) -> Fallible<()> {
    for (at, (_, categories)) in SMALL.iter().enumerate() {
        let space = Key::new(account, format!("30000000000000000{}", at + 2));
        let mut groups = Vec::new();
        let mut rows = Vec::new();
        for (category, channels) in *categories {
            let group = format!("36000000000000{at}{:03}", groups.len());
            groups.push(Group::new(group.clone(), *category, rows.len() as i32));
            for name in *channels {
                rows.push(ConversationRow {
                    group: Some(group.clone()),
                    position: rows.len() as i32,
                    // Not opted in to: listed only under "Browse more channels".
                    opted_out: at == 0 && *category == "Deep ends",
                    ..ConversationRow::new(format!("41000000000000{at}{:03}", rows.len()), Channel, *name)
                });
            }
        }
        s.store.replace_tree(&space, &groups, &rows)?;
        let general = Key::new(account, rows[0].id.clone());
        if at > 0 {
            s.say(&general, &cast[(at * 3) % 20], 600 * at as i64, "Quiet week. Anyone building anything?")?;
            s.say(&general, &cast[(at * 5) % 20], 590 * at as i64, "Slowly. Photos when it stops looking like a fire hazard.")?;
            // One server is unread, one has a mention, the rest are read.
            s.counts(&general, u32::from(at < 3) * 2, u32::from(at == 2), None)?;
            continue;
        }
        // As many as retention keeps: a longer history would be cut to this at the next start.
        let mut minutes = crate::store::KEEP_MESSAGES as i64 * 14;
        let mut speaker = 0;
        for line in 0..crate::store::KEEP_MESSAGES {
            if s.roll(3) == 0 {
                speaker = s.roll(12) as usize;
            }
            let (opener, topic, closer) = (s.roll(12) as usize, s.roll(12) as usize, s.roll(10) as usize);
            let text = format!("{} {}. {} (#{})", OPENERS[opener], TOPICS[topic], CLOSERS[closer], line + 1);
            s.say(&general, &cast[speaker], minutes, &text)?;
            minutes = (minutes - 1 - s.roll(26) as i64).max(1);
        }
    }
    Ok(())
}

fn direct(s: &mut Seeder, account: &AccountKey, cast: &[Who], me: &Who) -> Fallible<Key> {
    let open = |s: &mut Seeder, at: usize| {
        let who = &cast[at];
        s.conversation(account, ConversationRow {
            avatar: who.avatar.as_ref().map(|file| file.id.clone()),
            peer: Some(who.id.clone()),
            call: CallSupport::Native,
            ..ConversationRow::new(format!("500000000000000{:03}", at + 1), ConversationKind::Direct, who.name.clone())
        })
    };

    // Unread: a number.
    let ines = open(s, INES)?;
    let said = s.script(&ines, cast, me, &[
        (INES, 2890, "Did you see the frost flowers by the weir this morning?"),
        (YOU, 2885, "Missed them. I was at the wrong weir."),
        (INES, 2884, "There is only one weir."),
        (YOU, 2880, "Then I was at the wrong river."),
        (INES, 9, "Van is booked for Saturday, 7:30 at the usual corner."),
        (INES, 8, "Can you bring the trail camera and the long tripod?"),
        (INES, 7, "And tell Tomasz, he never reads the channel."),
    ])?;
    s.counts(&ines, 3, 0, Some(&said[3]))?;

    // Read, and we spoke last.
    let tomasz = open(s, TOMASZ)?;
    s.script(&tomasz, cast, me, &[
        (TOMASZ, 1500, "Do you still have my soldering iron?"),
        (YOU, 1490, "I do. It is on my desk, judging me."),
        (TOMASZ, 1488, "Bring it Saturday and all is forgiven."),
        (YOU, 70, "Ines says 7:30 at the usual corner. Bring batteries."),
    ])?;

    // No history at all.
    open(s, WREN)?;

    // Muted and unread: dimmed, no badge.
    let odalys = open(s, ODALYS)?;
    s.script(&odalys, cast, me, &[
        (ODALYS, 5000, "Sending you the draft key for the Cladonia section."),
        (YOU, 4990, "Got it. I will read it on the train."),
        (ODALYS, 260, "Page 4 has a mistake in couplet 12, ignore it."),
        (ODALYS, 255, "And page 9. Ignore page 9 entirely."),
    ])?;
    s.counts(&odalys, 2, 0, None)?;
    s.store.set_muted(&odalys, true)?;

    // Muted with a mention: a quiet number.
    let mirela = open(s, MIRELA)?;
    s.script(&mirela, cast, me, &[
        (MIRELA, 3100, "New mix is up if you want something for the drive."),
        (YOU, 3000, "Listening now. Track three is doing a lot."),
    ])?;
    let ping = s.message(account, &cast[MIRELA], 95, "@Robin Vale you left your gloves in my car.");
    s.post(&mirela, &ping)?;
    s.counts(&mirela, 1, 1, None)?;
    s.store.set_muted(&mirela, true)?;

    // Read, older, and no picture: initials.
    let kofi = open(s, KOFI)?;
    s.script(&kofi, cast, me, &[
        (KOFI, 12000, "Thanks for the lift back. I owe you a coffee."),
        (YOU, 11990, "You owe me three. I am keeping count."),
    ])?;

    // Read. `live` keeps this one moving.
    let yusuf = open(s, YUSUF)?;
    s.script(&yusuf, cast, me, &[
        (YUSUF, 210, "Firmware for the trail camera is nearly there. Motion trigger works, timestamps are wrong by an hour."),
        (YOU, 205, "Daylight saving?"),
        (YUSUF, 204, "Daylight saving."),
    ])?;

    // Group conversations: one unread (a number), one read.
    let carpool = s.conversation(account, ConversationRow {
        call: CallSupport::Native,
        ..ConversationRow::new("510000000000000001", ConversationKind::Group, "Foray carpool")
    })?;
    let said = s.script(&carpool, cast, me, &[
        (DITA, 400, "Who is driving on Saturday?"),
        (YOU, 395, "Ines has the van. I can take three more."),
        (HARUTO, 60, "I need a seat and space for a tripod."),
        (DITA, 58, "Same, minus the tripod."),
        (INES, 50, "Van has seven seats. Robin, you take the gear."),
        (HARUTO, 44, "Leaving from the usual corner?"),
        (INES, 43, "Usual corner, 7:30."),
        (DITA, 40, "I will bring pastries."),
        (HARUTO, 39, "This is why we invite Dita."),
        (INES, 30, "Weather says dry until two."),
        (DITA, 22, "Boots anyway."),
        (HARUTO, 21, "Always boots."),
        (INES, 16, "Robin, confirm you have the camera?"),
        (DITA, 15, "And the thermos."),
    ])?;
    s.counts(&carpool, 12, 0, Some(&said[1]))?;

    let band = s.conversation(account, ConversationRow {
        call: CallSupport::Native,
        ..ConversationRow::new("510000000000000002", ConversationKind::Group, "Kofi Asante, Mirela Dragan, Wren Halloran")
    })?;
    s.script(&band, cast, me, &[
        (MIRELA, 8000, "Rehearsal moved to Wednesday."),
        (KOFI, 7990, "Works for me."),
        (YOU, 7985, "I will bring the cables this time."),
    ])?;
    Ok(yusuf)
}

/// `many`: enough more servers to make fifty, each a `#general` and two channels, a few unread.
fn many(s: &mut Seeder, account: &AccountKey, cast: &[Who], spaces: &mut Vec<SpaceRow>) -> Fallible<()> {
    let named = spaces.len();
    for at in named..50 {
        let id = format!("3100000000000000{at:02}");
        let look = s.look(Art::Badge);
        let icon = s.picture(account, &format!("icon-many-{at}"), MediaKind::Icon, look, 96, 96)?;
        spaces.push(SpaceRow {
            id,
            name: format!("{} {}", FIRST[at % FIRST.len()], SECOND[at % SECOND.len()]),
            icon: Some(icon.id),
            position: at as i32,
        });
    }
    s.store.replace_spaces(account, spaces)?;
    for (at, space) in spaces.iter().enumerate().skip(named) {
        let group = format!("3700000000000000{at:02}");
        let rows: Vec<ConversationRow> = ["general", "links", "off-topic"]
            .iter()
            .enumerate()
            .map(|(position, name)| ConversationRow {
                group: Some(group.clone()),
                position: position as i32,
                ..ConversationRow::new(format!("42000000000000{at:02}{position:02}"), Channel, *name)
            })
            .collect();
        s.store.replace_tree(&Key::new(account, space.id.clone()), &[Group::new(group.clone(), "Text", 0)], &rows)?;
        let general = Key::new(account, rows[0].id.clone());
        s.say(&general, &cast[at % 20], 90 * at as i64, "Anyone around this week?")?;
        s.counts(&general, u32::from(at % 5 == 0) * 3, u32::from(at % 11 == 0), None)?;
    }
    Ok(())
}

pub fn seed(s: &mut Seeder) -> Fallible<Discord> {
    let account = account();
    s.store.set_account(&account, "Robin Vale", ME)?;
    let look = s.look(Art::Person);
    let mine = s.picture(&account, "avatar-me-5f1c2a9e", MediaKind::Avatar, look, 80, 80)?;
    let me = Who {
        id: ME.to_owned(),
        name: "Robin Vale".to_owned(),
        avatar: Some(mine),
        bot: false,
        me: true,
    };
    s.myself(&account, &me)?;
    let cast = s.people(&account, "220000000000000", PEOPLE)?;

    let names = std::iter::once("Moss & Lichen").chain(SMALL.iter().map(|(name, _)| *name));
    let mut spaces = Vec::new();
    for (at, name) in names.enumerate() {
        let icon = format!("icon-{at}-{:08x}", s.roll(u32::MAX as u64));
        // The last server's icon has not been fetched yet.
        let look = s.look(Art::Badge);
        let icon = match at {
            5 => s.waiting(&account, &icon, MediaKind::Icon, look, 96, 96)?,
            _ => s.picture(&account, &icon, MediaKind::Icon, look, 96, 96)?,
        };
        spaces.push(SpaceRow {
            id: format!("30000000000000000{}", at + 1),
            name: name.to_owned(),
            icon: Some(icon.id),
            position: at as i32,
        });
    }
    s.store.replace_spaces(&account, &spaces)?;

    moss(s, &account, &cast, &me)?;
    small(s, &account, &cast)?;
    if s.mode.many {
        many(s, &account, &cast, &mut spaces)?;
    }
    let live = direct(s, &account, &cast, &me)?;
    // Campfire is the 35th row of the Moss & Lichen tree.
    let voice = Key::new(&account, CAMPFIRE);
    Ok(Discord { account, me, cast, live, voice })
}
