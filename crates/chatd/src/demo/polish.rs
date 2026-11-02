//! What the polish round added to the Discord and Slack demo accounts, what every demo account
//! may send, and the pickers' catalogs.

use super::discord::{self, Discord, HARUTO, INES, MIRELA, TOMASZ, WREN};
use super::png::Art;
use super::{slack, teams, telegram, Fallible, Seeder, Who};
use crate::store::{ConversationRow, Filing, FolderRow, PersonDetails};
use protocol::{
    AccountKey, Activity, ActivityKind, CallSupport, Capabilities, CatalogItem, CatalogKind, ConversationKind,
    EmojiScope, Key, Mark, MediaKind, Special,
};

/// The pickers' catalogs: (id, name, pack). Files are registered at seed time; the demo's
/// adapter answers `Catalog` from these.
pub const EMOJI: &[(&str, &str, &str)] = &[
    ("emoji-mossparrot", ":mossparrot:", "Moss & Lichen"),
    ("emoji-lichen_ok", ":lichen_ok:", "Moss & Lichen"),
    ("emoji-sporeprint", ":sporeprint:", "Moss & Lichen"),
    ("emoji-fernwave", ":fernwave:", "Moss & Lichen"),
    ("emoji-trailboot", ":trailboot:", "Moss & Lichen"),
    ("emoji-campfire", ":campfire:", "Moss & Lichen"),
];
/// Harbor Labs' own emoji, as Slack lists a workspace's custom set.
pub const SLACK_EMOJI: &[(&str, &str, &str)] = &[
    ("emoji-shipit", ":shipit:", "Harbor Labs"),
    ("emoji-lgtm", ":lgtm:", "Harbor Labs"),
    ("emoji-anchor_wave", ":anchor_wave:", "Harbor Labs"),
    ("emoji-tidepool", ":tidepool:", "Harbor Labs"),
    ("emoji-lighthouse", ":lighthouse:", "Harbor Labs"),
    ("emoji-this", ":this:", "Harbor Labs"),
];
pub const STICKERS: &[(&str, &str, &str)] = &[
    ("sticker-fox-wave", "👋", "Forest Friends"),
    ("sticker-fox-sleep", "😴", "Forest Friends"),
    ("sticker-owl-wow", "😮", "Forest Friends"),
    ("sticker-owl-ok", "👌", "Forest Friends"),
    ("sticker-hedgehog-heart", "❤️", "Forest Friends"),
    ("sticker-hedgehog-sad", "😢", "Forest Friends"),
];
/// Discord's picker shows Favorites, then Trending; Telegram's answer names them "Saved"
/// (`adapter.rs`), and a search has no sections.
pub const GIFS: &[(&str, &str, &str)] = &[
    ("gif-cat-typing", "cat typing", "Favorites"),
    ("gif-cat-nap", "cat nap", "Favorites"),
    ("gif-dog-wave", "dog wave", "Favorites"),
    ("gif-thumbs-up", "thumbs up", "Trending"),
    ("gif-horse", "horse gallop", "Trending"),
    ("gif-greyhound", "greyhound", "Trending"),
    ("gif-mushroom-dance", "mushroom dance", "Trending"),
    ("gif-pigeons", "pigeons", "Trending"),
    ("gif-coffee", "coffee", "Trending"),
    ("gif-jump", "horse jump", "Trending"),
    ("gif-lion", "lion walk", "Trending"),
    ("gif-shrug", "shrug", "Trending"),
];
/// The picker GIFs that are real moving files in `photos/`, and their sizes. Only Discord's
/// picker shows them moving: Telegram's GIFs are MP4s, which a cell shows as a still.
const MOVING: &[(&str, &str, u32, u32)] = &[
    ("gif-horse", "gif-horse.gif", 210, 140),
    ("gif-greyhound", "gif-greyhound.webp", 480, 146),
    ("gif-pigeons", "pigeons.webp", 400, 147),
    ("gif-jump", "gif-jump.webp", 320, 120),
    ("gif-lion", "gif-lion.webp", 320, 95),
];

/// GIFs come in every shape: their stills are drawn at these sizes, in turn.
const GIF_SIZES: &[(u32, u32)] = &[(220, 124), (160, 160), (200, 150), (240, 100), (140, 180), (220, 160)];

/// The catalog an account answers, if it has that kind at all.
pub fn catalog(account: &AccountKey, kind: CatalogKind) -> &'static [(&'static str, &'static str, &'static str)] {
    use protocol::Network::{Discord, Slack, Telegram};
    match (account.network, kind) {
        (Discord, CatalogKind::Emoji) => EMOJI,
        (Slack, CatalogKind::Emoji) => SLACK_EMOJI,
        (Discord | Telegram, CatalogKind::Stickers) => STICKERS,
        (Discord | Telegram, CatalogKind::Gifs) => GIFS,
        _ => &[],
    }
}

/// One page of a catalog, filtered by what was typed, eight at a time.
pub fn page(files: &[protocol::MediaFile], kind: CatalogKind, items: &[(&str, &str, &str)], query: &str, cursor: Option<&str>) -> (Vec<CatalogItem>, Option<String>) {
    const PAGE: usize = 8;
    let query = query.to_lowercase();
    let wanted: Vec<_> = items.iter().filter(|(_, name, _)| query.is_empty() || name.to_lowercase().contains(&query)).collect();
    let from = cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0usize);
    let items = wanted
        .iter()
        .skip(from)
        .take(PAGE)
        .map(|(id, name, pack)| CatalogItem {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            file: files.iter().find(|file| file.id == *id).cloned(),
            pack: (*pack).to_owned(),
            animated: kind == CatalogKind::Gifs,
        })
        .collect();
    let next = (from + PAGE < wanted.len()).then(|| (from + PAGE).to_string());
    (items, next)
}

fn registered(s: &mut Seeder, account: &AccountKey, items: &[(&str, &str, &str)], kind: MediaKind, art: Art, size: u32) -> Fallible<()> {
    for (id, ..) in items {
        // Two are on disk already (the showcase's); the rest land when the picker shows them.
        if s.store.media(account, &[(*id).to_owned()])?.is_empty() {
            let look = s.look(art);
            s.waiting(account, id, kind, look, size, size)?;
        }
    }
    Ok(())
}

fn activity(kind: ActivityKind, name: &str, place: &str, conversation: Option<Key>) -> Option<Activity> {
    Some(Activity { kind, name: name.to_owned(), place: place.to_owned(), conversation })
}

pub fn seed(s: &mut Seeder, discord: &Discord) -> Fallible<()> {
    discord_extras(s, discord)?;
    slack_extras(s)?;
    for account in [discord::account(), telegram::account()] {
        registered(s, &account, STICKERS, MediaKind::Image, Art::Blob, 160)?;
        for (at, (id, ..)) in GIFS.iter().enumerate() {
            if s.store.media(&account, &[(*id).to_owned()])?.is_empty() {
                let look = s.look(Art::Scene);
                let moving = MOVING.iter().find(|(gif, ..)| gif == id).filter(|_| account.network == protocol::Network::Discord);
                match moving {
                    Some((_, file, width, height)) => drop(s.photo_waiting(&account, id, MediaKind::Image, file, look, *width, *height)?),
                    None => {
                        let (width, height) = GIF_SIZES[at % GIF_SIZES.len()];
                        s.waiting(&account, id, MediaKind::Gif, look, width, height)?;
                    }
                }
            }
        }
    }
    registered(s, &discord::account(), EMOJI, MediaKind::Emoji, Art::Blob, 48)?;
    registered(s, &slack::harbor(), SLACK_EMOJI, MediaKind::Emoji, Art::Blob, 48)?;

    // What each account may send, as its session would say.
    let mb = 1 << 20;
    s.capabilities.push((discord::account(), Capabilities {
        max_file_bytes: 10 * mb,
        max_files: 10,
        spoiler: true,
        gifs: true,
        stickers: true,
        custom_emoji: EmojiScope::Space,
        silent: true,
        suppress_embeds: true,
        reply_ping: true,
        ..Capabilities::default()
    }));
    for account in [slack::harbor(), slack::fernhill()] {
        s.capabilities.push((account, Capabilities {
            max_file_bytes: 1 << 30,
            max_files: 10,
            custom_emoji: EmojiScope::All,
            schedule: true,
            suppress_embeds: true,
            ..Capabilities::default()
        }));
    }
    s.capabilities.push((telegram::account(), Capabilities {
        max_file_bytes: 2 << 30,
        max_files: 10,
        caption_limit: 1_024,
        spoiler: true,
        as_file: true,
        gifs: true,
        stickers: true,
        silent: true,
        schedule: true,
        suppress_embeds: true,
        edit_window: 48 * 3_600,
        ..Capabilities::default()
    }));
    s.capabilities.push((teams::account(), Capabilities { max_file_bytes: 250 * mb, max_files: 10, ..Capabilities::default() }));
    Ok(())
}

/// Server folders, friends in voice and at play, a message request, an app's DM, a followed
/// thread.
fn discord_extras(s: &mut Seeder, discord: &Discord) -> Fallible<()> {
    let account = &discord.account;
    let space = |n: u32| format!("30000000000000000{n}");
    s.store.replace_folders(account, &[
        FolderRow { id: "folder-nature".into(), name: "Nature".into(), position: 0, color: Some(0x3b_a5_5c), spaces: vec![space(1), space(8)], ..FolderRow::default() },
        FolderRow { id: "folder-code".into(), name: "Code".into(), position: 1, color: Some(0x58_65_f2), spaces: vec![space(2), space(6), space(4)], ..FolderRow::default() },
        // Unnamed: the shell names it by its first servers.
        FolderRow { id: "folder-3".into(), name: String::new(), position: 2, spaces: vec![space(3), space(5)], ..FolderRow::default() },
    ])?;
    s.store.set_space_flags(&Key::new(account, space(3)), true, false)?;
    s.store.set_space_flags(&Key::new(account, space(1)), false, true)?;

    let campfire = Key::new(account, discord::CAMPFIRE);
    let cast = &discord.cast;
    let at = |who: usize| Key::new(account, cast[who].id.clone());
    for who in [INES, TOMASZ, WREN] {
        s.activities.push((at(who), String::new(), activity(ActivityKind::InVoice, "Campfire", "Moss & Lichen", Some(campfire.clone()))));
    }
    s.activities.push((at(discord::YUSUF), String::new(), activity(ActivityKind::InVoice, "Field recording", "Moss & Lichen", None)));
    // In a call of her own, not a voice channel: "In a call" on her DM row and in People.
    s.activities.push((at(discord::ODALYS), String::new(), activity(ActivityKind::InCall, "", "", None)));
    s.activities.push((at(HARUTO), String::new(), activity(ActivityKind::Playing, "Hades", "", None)));
    s.activities.push((at(MIRELA), "🎧".to_owned(), activity(ActivityKind::Listening, "Spotify", "", None)));
    s.activities.push((at(discord::DITA), "🌿".to_owned(), None));

    // The thread under #showcase is one we follow.
    s.store.set_filing(&Key::new(account, discord::BUILD_LOG), &Filing { followed: true, ..Filing::default() })?;

    // A stranger's message request: never counted, behind its own row.
    let stranger = cast.iter().find(|who| who.name == "Casimir Thorne").cloned().unwrap_or_else(|| cast[0].clone());
    let request = s.conversation(account, ConversationRow {
        peer: Some(stranger.id.clone()),
        avatar: stranger.avatar.as_ref().map(|file| file.id.clone()),
        special: Special::Request,
        ..ConversationRow::new("520000000000000001", ConversationKind::Direct, stranger.name.clone())
    })?;
    s.say(&request, &stranger, 50, "Hi! Saw your post about the herbarium scans, could I ask about the scanner?")?;
    s.counts(&request, 1, 0, None)?;

    // A bot's DM: an app, not a person.
    let bot = cast[discord::BOT].clone();
    let app = s.conversation(account, ConversationRow {
        peer: Some(bot.id.clone()),
        bot: true,
        ..ConversationRow::new("520000000000000002", ConversationKind::Direct, bot.name.clone())
    })?;
    s.say(&app, &bot, 25, "Daily digest: 12 new specimen posts, 3 need an ID.")?;
    s.counts(&app, 2, 0, None)
}

/// App DMs among a workspace's direct conversations, and people's titles and time zones.
fn slack_extras(s: &mut Seeder) -> Fallible<()> {
    let account = slack::harbor();
    let app = |id: &str, name: &str| Who { id: id.to_owned(), name: name.to_owned(), avatar: None, bot: true, me: false };
    let mut apps = [app("U04MEMBER008", "Deploy Bot"), app("B04GITHUB", "GitHub"), app("USLACKBOT", "Slackbot")];
    for who in &mut apps[1..] {
        if let Some(file) = super::photos::portrait(&who.name) {
            who.avatar = Some(s.photo(&account, &format!("avatar-{}", who.id), MediaKind::Avatar, &file, (Art::Badge, 0), 192, 192)?);
        }
    }
    for who in &apps[1..] {
        s.store.upsert_people(&account, &[super::person_row(&account, who, "", "")])?;
    }
    for (at, who) in apps.iter().enumerate() {
        let key = s.conversation(&account, ConversationRow {
            space: Some(account.account.clone()),
            group: Some("S04DIRECTS".to_owned()),
            position: 200 + at as i32,
            peer: Some(who.id.clone()),
            bot: true,
            call: CallSupport::None,
            ..ConversationRow::new(format!("D04APP{at:04}"), ConversationKind::Direct, who.name.clone())
        })?;
        let text = ["api 4f2c1a9 deployed to production", "Review requested on harbor/api#412", "Reminder: fill in your timesheet"][at];
        s.say(&key, who, 30 + at as i64 * 200, text)?;
        s.counts(&key, u32::from(at < 2), 0, None)?;
    }
    let titled = [
        ("U04MEMBER001", "Engineering manager", -18_000, None),
        ("U04MEMBER002", "Staff engineer", 19_800, None),
        ("U04MEMBER003", "Product lead", 3_600, None),
        ("U04MEMBER004", "Site reliability engineer", -25_200, None),
        ("U04MEMBER005", "Product designer", 3_600, None),
        ("U04MEMBER006", "Contractor", 0, Some(Mark::Guest)),
        ("U04MEMBER007", "Partner success", 0, Some(Mark::External)),
    ];
    let details: Vec<(String, PersonDetails)> = titled
        .iter()
        .map(|(id, title, offset, mark)| ((*id).to_owned(), PersonDetails { title: (*title).to_owned(), utc_offset: Some(*offset), mark: *mark }))
        .collect();
    s.store.set_details(&account, &details)?;
    s.activities.push((Key::new(&account, "U04MEMBER007"), "🌴".to_owned(), None));
    s.activities.push((Key::new(&account, "U04MEMBER003"), "📅".to_owned(), activity(ActivityKind::Huddle, "eng-incidents", "Harbor Labs", None)));
    Ok(())
}
