//! The demo's Telegram account: one flat chat list with no Space, as Telegram has it. Pins,
//! folders that filter it, an archive, a channel with a sponsored message, a forum with topics,
//! Saved Messages, the service chat that brings login codes, a bot, and a scam account.

use super::png::Art;
use super::{Fallible, Listed, Seeder, Who};
use crate::store::{ConversationRow, Filing, FolderRow, PersonDetails, PreviewLine};
use protocol::{AccountKey, Card, ConversationKind, Key, Mark, MediaKind, Network, Presence, Reaction, Relation, Special};
use ConversationKind::{Announcement, Direct, Forum, Group, Thread};
use Presence::{Offline, Online, Unknown};
use Relation::Contact;

pub const ME: &str = "u7000000";
/// Telegram's own account, which sends login codes.
pub const SERVICE: &str = "u777000";
pub const CHANNEL: &str = "c9001";
pub const FORUM: &str = "c9100";
/// Telegram's own reaction emoji, as `messages.getAvailableReactions` lists them for an account
/// without Premium: the only ones a chat can allow.
pub const REACTIONS: &[&str] = &[
    "👍", "👎", "❤", "🔥", "🥰", "👏", "😁", "🤔", "🤯", "😱", "🤬", "😢", "🎉", "🤩", "🤮", "💩", "🙏",
    "👌", "🕊", "🤡", "🥱", "🥴", "😍", "🐳", "❤\u{200d}🔥", "🌚", "🌭", "💯", "🤣", "⚡", "🍌", "🏆", "💔",
    "🤨", "😐", "🍓", "🍾", "💋", "🖕", "😈", "😴", "😭", "🤓", "👻", "👨\u{200d}💻", "👀", "🎃", "🙈",
    "😇", "😨", "🤝", "✍", "🤗", "🫡", "🎅", "🎄", "☃", "💅", "🤪", "🗿", "🆒", "💘", "🙉", "🦄", "😘",
    "💊", "🙊", "😎", "👾", "🤷\u{200d}♂", "🤷", "🤷\u{200d}♀", "😡",
];

pub fn account() -> AccountKey {
    AccountKey {
        network: Network::Telegram,
        account: "7000000".to_owned(),
    }
}

const CONTACTS: &[Listed] = &[
    ("Ines Marchetti", "ines_m", Online, "", Contact),
    ("Mara Okafor", "maraok", Offline, "", Contact),
    // Hides when last seen: Telegram's own words, never a dot.
    ("Haruto Senda", "haruto", Unknown, "last seen recently", Contact),
    ("Lena Brandt", "lenab", Online, "", Contact),
    ("Pavel Orlov", "", Unknown, "last seen within a week", Contact),
    ("Grandma Rosa", "", Offline, "", Contact),
    ("Weather Bot", "weatherbot", Unknown, "", Relation::None),
    ("Crypto Giveaway Official", "free_crypto_drop", Unknown, "", Relation::None),
];
const INES: usize = 0;
const MARA: usize = 1;
const HARUTO: usize = 2;
const LENA: usize = 3;
const PAVEL: usize = 4;
const ROSA: usize = 5;
const WEATHER: usize = 6;
const SCAM: usize = 7;

/// Puts reactions on a message already recorded.
fn reacted(s: &mut Seeder, conversation: &Key, id: &str, list: &[(&str, u32, bool)]) -> Fallible<()> {
    if let Some(mut message) = s.store.message(conversation, id)? {
        message.reactions = list.iter().map(|(emoji, count, mine)| Reaction { emoji: (*emoji).to_owned(), file: None, count: *count, mine: *mine }).collect();
        s.post(conversation, &message)?;
    }
    Ok(())
}

fn folders(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

pub fn seed(s: &mut Seeder) -> Fallible<Who> {
    let account = account();
    s.store.set_account(&account, "Robin Vale", ME)?;
    let me = Who {
        id: ME.to_owned(),
        name: "Robin Vale".to_owned(),
        avatar: Some(s.avatar(&account, "tg-avatar-me", "Robin Vale")?),
        bot: false,
        me: true,
    };
    s.myself(&account, &me)?;
    let cast = s.people(&account, "u7000", CONTACTS)?;
    s.store.set_details(&account, &[
        (cast[SCAM].id.clone(), PersonDetails { mark: Some(Mark::Scam), ..PersonDetails::default() }),
        (cast[INES].id.clone(), PersonDetails { utc_offset: Some(3_600), ..PersonDetails::default() }),
    ])?;
    let service = Who {
        id: SERVICE.to_owned(),
        name: "Telegram".to_owned(),
        avatar: Some(s.photo(&account, "tg-avatar-777000", MediaKind::Avatar, "avatar-telegram.png", (Art::Badge, 200), 192, 192)?),
        bot: false,
        me: false,
    };
    s.store.upsert_people(&account, &[super::person_row(&account, &service, "telegram", "")])?;
    s.store.set_details(&account, &[(SERVICE.to_owned(), PersonDetails { mark: Some(Mark::Verified), ..PersonDetails::default() })])?;

    // A person's call opens in Telegram's own app, as the adapter says of every person's chat;
    // bots and Telegram's own account have none.
    let direct = |who: &Who, filing: Filing| ConversationRow {
        avatar: who.avatar.as_ref().map(|file| file.id.clone()),
        peer: Some(who.id.clone()),
        filing: Some(filing),
        call: if who.bot || who.id == SERVICE { protocol::CallSupport::None } else { protocol::CallSupport::Handoff },
        ..ConversationRow::new(who.id.clone(), Direct, who.name.clone())
    };
    let talk = |s: &mut Seeder, row: ConversationRow, lines: &[(usize, i64, &str)]| -> Fallible<(Key, Vec<String>)> {
        let key = s.conversation(&account, row)?;
        let said = s.script(&key, &cast, &me, lines)?;
        Ok((key, said))
    };

    // Saved Messages: pinned first, ours alone.
    talk(s, ConversationRow {
        special: Special::Saved,
        filing: Some(Filing { pin: Some(0), ..Filing::default() }),
        ..ConversationRow::new(ME, Direct, "Saved Messages")
    }, &[(usize::MAX, 4000, "Locker code 4417"), (usize::MAX, 30, "Buy: film, batteries, chalk")])?;

    // Pinned, unread.
    let (ines, said) = talk(s, direct(&cast[INES], Filing { pin: Some(1), folders: folders(&["work"]), ..Filing::default() }), &[
        (usize::MAX, 600, "Did the herbarium send the scans?"),
        (INES, 40, "They did. Uploading them tonight."),
        (INES, 38, "Also: are you in for Sunday?"),
    ])?;
    s.counts(&ines, 2, 0, Some(&said[0]))?;
    // A voice note between the two, as Telegram sends one: Ogg Opus with its waveform.
    if let Some(bytes) = super::photos::bytes("voice-ines.opus") {
        let mut note = s.message(&account, &cast[INES], 39, "");
        let file = s.file(&account, "tg-voice-ines", MediaKind::Audio, bytes.len() as u64, "photo/voice-ines.opus")?;
        note.attachments.push(protocol::Attachment {
            mime: "audio/ogg".to_owned(),
            duration: 6,
            waveform: super::showcase::waveform(64),
            ..protocol::Attachment::new("Voice message", file)
        });
        s.post(&ines, &note)?;
        s.counts(&ines, 3, 0, Some(&said[0]))?;
    }

    // Ours last, and read by her: ✓✓.
    let (mara, said) = talk(s, direct(&cast[MARA], Filing { folders: folders(&["work"]), ..Filing::default() }), &[
        (MARA, 300, "Can you send me the slides?"),
        (usize::MAX, 280, "Sent. Shout if the fonts are off."),
    ])?;
    s.store.set_seen(&mara, &said[1])?;

    // Ours last, not read yet: ✓.
    talk(s, direct(&cast[LENA], Filing { folders: folders(&["work"]), ..Filing::default() }), &[
        (LENA, 900, "Which train are you on?"),
        (usize::MAX, 25, "The 18:40, carriage 4."),
    ])?;
    talk(s, direct(&cast[HARUTO], Filing::default()), &[(HARUTO, 3000, "Thanks for the lens!"), (usize::MAX, 2990, "Any time.")])?;
    talk(s, direct(&cast[PAVEL], Filing::default()), &[(PAVEL, 12_000, "Long time. Still in Lisbon?")])?;
    // In two folders: listed, and counted, in both tabs.
    let (rosa, _) = talk(s, direct(&cast[ROSA], Filing { folders: folders(&["family", "work"]), ..Filing::default() }), &[
        (ROSA, 70, "Sunday lunch at one. Bring bread."),
    ])?;
    s.counts(&rosa, 1, 0, None)?;

    // A bot: no presence, folds into Apps in the Inbox.
    let (weather, _) = talk(s, ConversationRow { bot: true, ..direct(&cast[WEATHER], Filing::default()) }, &[
        (WEATHER, 20, "Lisbon: 21°, sun, wind 12 km/h from the north."),
    ])?;
    s.counts(&weather, 1, 0, None)?;
    // A stranger Telegram itself marks as a scam.
    talk(s, ConversationRow { mark: Some(Mark::Scam), ..direct(&cast[SCAM], Filing::default()) }, &[
        (SCAM, 15, "Congratulations! You won 2 BTC. Send 0.1 to claim."),
    ])?;
    // The service chat: login codes arrive here; read-only, verified, never a call.
    let (service_chat, _) = talk(s, ConversationRow {
        special: Special::Service,
        mark: Some(Mark::Verified),
        read_only: true,
        ..direct(&service, Filing::default())
    }, &[])?;
    s.say(&service_chat, &service, 2, "Login code: 51923. Do not give this code to anyone, even if they say they are from Telegram!")?;
    s.counts(&service_chat, 1, 0, None)?;

    // Groups: who spoke last shows in the preview. Family is pinned inside its folder.
    let avatar = s.cover(&account, "g8001", "group-foraging-lisbon.webp")?;
    let (foraging, said) = talk(s, ConversationRow {
        listed: true,
        avatar,
        filing: Some(Filing { folders: folders(&["work"]), ..Filing::default() }),
        ..ConversationRow::new("g8001", Group, "Foraging Lisbon")
    }, &[
        (INES, 500, "Chanterelles at Sintra, north side of the ridge."),
        (LENA, 120, "Going Saturday. Room for two in the car."),
        (MARA, 18, "ok"),
    ])?;
    s.counts(&foraging, 2, 0, Some(&said[0]))?;
    // Its admins allow five reactions (`adapter.rs`); ours is one of them, and a second would
    // replace it, as Telegram has it without Premium.
    reacted(s, &foraging, &said[0], &[("🔥", 3, false), ("❤", 2, true)])?;
    let mut mention = s.message(&account, &cast[INES], 10, "@Robin Vale are you bringing the basket?");
    mention.mentions_me = true;
    s.post(&foraging, &mention)?;
    s.counts(&foraging, 3, 1, Some(&said[0]))?;
    let avatar = s.cover(&account, "g8002", "group-family.webp")?;
    talk(s, ConversationRow {
        avatar,
        filing: Some(Filing { folders: folders(&["family"]), ..Filing::default() }),
        ..ConversationRow::new("g8002", Group, "Family")
    }, &[(ROSA, 200, "Photos from the weekend 💛"), (usize::MAX, 190, "Lovely!")])?;

    // A channel: no sender in its preview, read-only, a sponsored message at its foot that is
    // never its preview.
    // A channel's posts are the channel's, not a person's.
    let poster = |name: &str| Who { id: String::new(), name: name.to_owned(), avatar: None, bot: false, me: false };
    let avatar = s.cover(&account, CHANNEL, "group-lichen-daily.webp")?;
    let (channel, _) = talk(s, ConversationRow {
        read_only: true,
        avatar,
        filing: Some(Filing { folders: folders(&["channels"]), ..Filing::default() }),
        ..ConversationRow::new(CHANNEL, Announcement, "Lichen Daily")
    }, &[])?;
    let said = [
        s.say(&channel, &poster("Lichen Daily"), 2000, "Xanthoria parietina: the orange one on every wall by the sea.")?,
        s.say(&channel, &poster("Lichen Daily"), 600, "Snow line is down to 1,200 m. Crustose lichens love it.")?,
    ];
    let newest = s.store.message(&channel, &said[1])?.map_or(s.now, |message| message.sent);
    let mut ad = protocol::Message::plain("adad01", "Larch Boots", "Resoled by hand in Innsbruck.", newest, false);
    ad.card = Some(Box::new(Card { title: "Sponsored".to_owned(), footer: "Larch Boots GmbH".to_owned(), ..Card::default() }));
    s.post(&channel, &ad)?;
    // The channel's own post is the preview, not the advertisement.
    if let Some(post) = s.store.message(&channel, &said[1])? {
        let line = PreviewLine { id: post.id.clone(), text: post.text.clone(), sender: String::new(), mine: false, sent: post.sent };
        s.store.set_preview(&channel, &line)?;
    }
    s.counts(&channel, 4, 0, Some(&said[0]))?;
    reacted(s, &channel, &said[1], &[("👍", 14, false), ("🔥", 6, false)])?;

    // A forum: topics under it, pinned first, one unread.
    let avatar = s.cover(&account, FORUM, "group-mycology.webp")?;
    s.conversation(&account, ConversationRow {
        avatar,
        filing: Some(Filing { folders: folders(&["work"]), ..Filing::default() }),
        ..ConversationRow::new(FORUM, Forum, "Mycology Forum")
    })?;
    for (id, title, pin, minutes, unread) in [("c9100-1", "General", Some(0), 400, 0), ("c9100-2", "ID requests", None, 5, 3), ("c9100-3", "Events", None, 900, 0)] {
        let topic = s.conversation(&account, ConversationRow {
            parent: Some(FORUM.to_owned()),
            filing: Some(Filing { pin, ..Filing::default() }),
            ..ConversationRow::new(id, Thread, title)
        })?;
        s.say(&topic, &cast[(minutes as usize) % 4], minutes, "Photos of the gills, please, and a spore print if you have one.")?;
        s.counts(&topic, unread, 0, None)?;
    }
    let forum = Key::new(&account, FORUM);
    s.say(&forum, &cast[LENA], 5, "ID requests: new photos of a bolete")?;

    // The archive: out of the Inbox; the mention in one still counts.
    let avatar = s.cover(&account, "c9002", "group-trail-conditions.webp")?;
    let (trail, _) = talk(s, ConversationRow {
        avatar,
        filing: Some(Filing { archived: true, folders: folders(&["channels"]), ..Filing::default() }),
        read_only: true,
        ..ConversationRow::new("c9002", Announcement, "Trail Conditions")
    }, &[])?;
    s.say(&trail, &poster("Trail Conditions"), 1500, "Ridge loop closed until April.")?;
    s.store.set_muted(&trail, true)?;
    let avatar = s.cover(&account, "g8003", "group-old-flatshare.webp")?;
    let (flat, said) = talk(s, ConversationRow {
        avatar,
        filing: Some(Filing { archived: true, ..Filing::default() }),
        ..ConversationRow::new("g8003", Group, "Old Flatshare")
    }, &[(PAVEL, 20_000, "Who took the good pan?"), (HARUTO, 19_000, "Not me.")])?;
    s.counts(&flat, 1, 0, Some(&said[0]))?;

    s.store.replace_folders(&account, &[
        FolderRow { id: "work".into(), name: "Work".into(), position: 0, ..FolderRow::default() },
        FolderRow { id: "family".into(), name: "Family".into(), position: 1, pinned: folders(&["g8002"]), ..FolderRow::default() },
        FolderRow { id: "channels".into(), name: "Channels".into(), position: 2, ..FolderRow::default() },
    ])?;
    Ok(me)
}
